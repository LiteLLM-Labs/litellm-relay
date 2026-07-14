use std::{env, fs, path::PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use toml_edit::{value, Array, DocumentMut, InlineTable, Item, Table, Value};

use crate::{
    ai_tools::token::ensure_token,
    config::{load_settings, save_settings, RelaySettings},
    system::home_dir,
};

/// Codex's only supported wire protocol as of codex-rs `rust-v0.144.4`: the
/// Responses API at `/v1/responses`. `wire_api = "chat"` is rejected by the
/// binary, so the Gateway must serve the OpenAI Responses API.
const WIRE_API: &str = "responses";

/// How often Codex proactively refreshes the bearer token, matching its own
/// default of five minutes so the short-lived identity token stays valid.
const TOKEN_REFRESH_INTERVAL_MS: i64 = 300_000;

/// Environment variable that overrides where the Codex config is written. Used
/// by tests so they never touch the developer's real `~/.codex/config.toml`.
const CODEX_CONFIG_PATH_ENV: &str = "LITELLM_RELAY_CODEX_CONFIG";

/// How Codex should obtain the Gateway bearer credential. These map onto the
/// mutually exclusive auth fields of Codex's `ModelProviderInfo`.
#[derive(Debug, Default)]
enum Credential<'a> {
    /// Command-backed `auth` hook running Relay's token helper (default). Codex
    /// fetches a short-lived identity token on demand; no key on the device.
    #[default]
    TokenHelper,
    /// Codex reads the bearer key from an environment variable (`env_key`).
    /// Relay's token helper is expected to populate it with the identity token.
    EnvKey(&'a str),
    /// Static gateway key embedded as `experimental_bearer_token`.
    StaticKey(&'a str),
}

/// Inputs for wiring Codex CLI to route through the Gateway. Supplied by the
/// onboarding command or interactively; any field left unset falls back to the
/// saved Relay config.
#[derive(Debug, Default)]
pub struct CodexOnboardParams {
    pub gateway_url: Option<String>,
    pub authorize_url: Option<String>,
    pub team: Option<String>,
    pub model: Option<String>,
    /// Have Codex read the bearer key from this env var instead of the token
    /// helper hook. Relay's token command is expected to populate it.
    pub env_key: Option<String>,
    /// Static gateway key fallback for environments without an IdP.
    pub api_key: Option<String>,
}

/// Writes `~/.codex/config.toml` so `codex` sends requests to the Gateway
/// through a custom OpenAI-compatible provider. By default the provider uses a
/// command-backed `auth` hook that runs Relay's token helper, so Codex obtains
/// a short-lived corporate-identity bearer token and no provider key ever
/// touches the device. `--env-key` and a static `--api-key` are alternatives.
pub fn onboard(params: CodexOnboardParams) -> Result<()> {
    let mut settings = load_settings()?;
    if let Some(gateway_url) = params.gateway_url {
        settings.gateway.url = gateway_url.trim_end_matches('/').to_string();
    }
    if let Some(authorize_url) = params.authorize_url {
        settings.idp.authorize_url = authorize_url;
    }
    if let Some(model) = params.model {
        settings.codex.model = model;
    }
    if params.team.is_some() {
        settings.codex.team = params.team;
    }

    let static_key = params
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty());
    let env_key = params
        .env_key
        .as_deref()
        .filter(|key| !key.trim().is_empty());
    if static_key.is_some() && env_key.is_some() {
        bail!("--api-key and --env-key are mutually exclusive");
    }
    let credential = match (static_key, env_key) {
        (Some(key), _) => Credential::StaticKey(key),
        (_, Some(var)) => Credential::EnvKey(var),
        _ => Credential::TokenHelper,
    };

    let needs_idp = matches!(credential, Credential::TokenHelper);
    if needs_idp && settings.idp.authorize_url.trim().is_empty() {
        bail!(
            "onboarding requires an IdP authorize URL (--authorize-url or idp.authorize_url), \
             or pass --env-key / --api-key for a static credential"
        );
    }

    let exe = relay_executable()?;
    let config_path = write_codex_config(&settings, &credential, &exe)?;
    save_settings(&settings)?;

    println!(
        "Codex is wired to {} (provider `{}`, model `{}`)",
        provider_base_url(&settings),
        settings.codex.provider_id,
        settings.codex.model
    );
    if let Some(team) = &settings.codex.team {
        println!("Team header: x-litellm-team: {team}");
    }
    match &credential {
        Credential::TokenHelper => {
            println!("Codex fetches a short-lived identity token via `relay codex-token`.");
        }
        Credential::EnvKey(var) => {
            println!(
                "Codex reads the bearer key from ${var}. Populate it with the identity token, e.g.\n  export {var}=\"$({exe} codex-token)\""
            );
        }
        Credential::StaticKey(_) => {
            println!("Using a static gateway key from --api-key.");
        }
    }
    println!("Wrote {}", config_path.display());
    println!("Run `codex` and sign in through your browser on first use.");
    Ok(())
}

/// Prints a valid IdP bearer token on stdout for Codex's `auth` command hook.
pub fn print_token() -> Result<()> {
    let settings = load_settings()?;
    let token = ensure_token(&settings.idp.authorize_url)?;
    println!("{token}");
    Ok(())
}

fn codex_config_path() -> PathBuf {
    if let Some(path) = env::var_os(CODEX_CONFIG_PATH_ENV) {
        return PathBuf::from(path);
    }
    home_dir().join(".codex").join("config.toml")
}

fn provider_base_url(settings: &RelaySettings) -> String {
    format!("{}/v1", settings.gateway.url.trim_end_matches('/'))
}

fn relay_executable() -> Result<String> {
    let exe = env::current_exe().context("failed to resolve the Relay executable path")?;
    exe.to_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Relay executable path is not valid UTF-8"))
}

fn write_codex_config(
    settings: &RelaySettings,
    credential: &Credential,
    exe: &str,
) -> Result<PathBuf> {
    let path = codex_config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let existing = if path.exists() {
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?
    } else {
        String::new()
    };
    let rendered = render_codex_config(&existing, settings, credential, exe)?;
    fs::write(&path, rendered).with_context(|| format!("failed to write {}", path.display()))?;
    secure_file(&path)?;
    Ok(path)
}

/// Merges Relay's Codex provider into an existing `config.toml`, preserving any
/// other providers and user settings. Returns the serialized document.
fn render_codex_config(
    existing: &str,
    settings: &RelaySettings,
    credential: &Credential,
    exe: &str,
) -> Result<String> {
    let mut doc = existing
        .parse::<DocumentMut>()
        .context("failed to parse existing ~/.codex/config.toml")?;

    let provider_id = settings.codex.provider_id.as_str();
    doc["model"] = value(settings.codex.model.clone());
    doc["model_provider"] = value(provider_id);

    if !doc.contains_key("model_providers") {
        let mut providers = Table::new();
        providers.set_implicit(true);
        doc["model_providers"] = Item::Table(providers);
    }
    doc["model_providers"][provider_id] =
        Item::Table(build_provider_table(settings, credential, exe));

    Ok(doc.to_string())
}

fn build_provider_table(settings: &RelaySettings, credential: &Credential, exe: &str) -> Table {
    let mut provider = Table::new();
    provider["name"] = value("LiteLLM AI Gateway");
    provider["base_url"] = value(provider_base_url(settings));
    provider["wire_api"] = value(WIRE_API);

    if let Some(team) = &settings.codex.team {
        let mut headers = InlineTable::new();
        headers.insert("x-litellm-team", Value::from(team.clone()));
        provider["http_headers"] = value(headers);
    }

    // Codex treats `auth`, `env_key`, and `experimental_bearer_token` as
    // mutually exclusive, so exactly one is written.
    match credential {
        Credential::TokenHelper => {
            let mut auth = Table::new();
            auth["command"] = value(exe);
            let mut args = Array::new();
            args.push("codex-token");
            auth["args"] = value(args);
            auth["refresh_interval_ms"] = value(TOKEN_REFRESH_INTERVAL_MS);
            provider["auth"] = Item::Table(auth);
        }
        Credential::EnvKey(var) => {
            provider["env_key"] = value(*var);
        }
        Credential::StaticKey(key) => {
            provider["experimental_bearer_token"] = value(*key);
        }
    }

    provider
}

fn secure_file(path: &PathBuf) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RelaySettings;

    fn settings_with_team(team: Option<&str>) -> RelaySettings {
        let mut settings = RelaySettings::default();
        settings.gateway.url = "https://gateway.example.com".into();
        settings.idp.authorize_url = "https://login.example.com/authorize".into();
        settings.codex.model = "gpt-5-codex".into();
        settings.codex.team = team.map(str::to_string);
        settings
    }

    fn parse(rendered: &str) -> toml_edit::DocumentMut {
        rendered
            .parse()
            .expect("rendered config should be valid TOML")
    }

    #[test]
    fn should_point_codex_at_gateway_and_select_provider() {
        let settings = settings_with_team(None);
        let rendered = render_codex_config(
            "",
            &settings,
            &Credential::TokenHelper,
            "/opt/relay/litellm-relay",
        )
        .unwrap();
        let doc = parse(&rendered);

        assert_eq!(doc["model"].as_str(), Some("gpt-5-codex"));
        assert_eq!(doc["model_provider"].as_str(), Some("litellm"));
        assert_eq!(
            doc["model_providers"]["litellm"]["base_url"].as_str(),
            Some("https://gateway.example.com/v1")
        );
        assert_eq!(
            doc["model_providers"]["litellm"]["wire_api"].as_str(),
            Some("responses")
        );
    }

    #[test]
    fn should_use_token_helper_and_not_write_static_key_on_sso_path() {
        let settings = settings_with_team(None);
        let rendered = render_codex_config(
            "",
            &settings,
            &Credential::TokenHelper,
            "/opt/relay/litellm-relay",
        )
        .unwrap();
        let doc = parse(&rendered);
        let provider = &doc["model_providers"]["litellm"];

        assert_eq!(
            provider["auth"]["command"].as_str(),
            Some("/opt/relay/litellm-relay")
        );
        assert_eq!(provider["auth"]["args"][0].as_str(), Some("codex-token"));
        assert!(provider["auth"]["refresh_interval_ms"].is_integer());
        assert!(
            provider.get("experimental_bearer_token").is_none(),
            "SSO path must not embed a static provider key"
        );
        assert!(provider.get("env_key").is_none());
    }

    #[test]
    fn should_write_env_key_and_no_auth_hook_on_env_key_path() {
        let settings = settings_with_team(None);
        let rendered = render_codex_config(
            "",
            &settings,
            &Credential::EnvKey("LITELLM_API_KEY"),
            "/opt/relay/litellm-relay",
        )
        .unwrap();
        let doc = parse(&rendered);
        let provider = &doc["model_providers"]["litellm"];

        assert_eq!(provider["env_key"].as_str(), Some("LITELLM_API_KEY"));
        assert!(provider.get("auth").is_none());
        assert!(provider.get("experimental_bearer_token").is_none());
    }

    #[test]
    fn should_embed_static_key_and_drop_auth_hook_on_static_path() {
        let settings = settings_with_team(None);
        let rendered = render_codex_config(
            "",
            &settings,
            &Credential::StaticKey("sk-static-123"),
            "/opt/relay/litellm-relay",
        )
        .unwrap();
        let doc = parse(&rendered);
        let provider = &doc["model_providers"]["litellm"];

        assert_eq!(
            provider["experimental_bearer_token"].as_str(),
            Some("sk-static-123")
        );
        assert!(
            provider.get("auth").is_none(),
            "static path must not configure the token helper hook"
        );
        assert!(provider.get("env_key").is_none());
    }

    #[test]
    fn should_add_team_header_when_team_set() {
        let settings = settings_with_team(Some("engineering"));
        let rendered = render_codex_config(
            "",
            &settings,
            &Credential::TokenHelper,
            "/opt/relay/litellm-relay",
        )
        .unwrap();
        let doc = parse(&rendered);

        assert_eq!(
            doc["model_providers"]["litellm"]["http_headers"]["x-litellm-team"].as_str(),
            Some("engineering")
        );
    }

    #[test]
    fn should_preserve_unrelated_existing_config() {
        let existing = "\
approval_policy = \"on-request\"

[model_providers.other]
name = \"Other\"
base_url = \"https://other.example.com/v1\"
";
        let settings = settings_with_team(None);
        let rendered = render_codex_config(
            existing,
            &settings,
            &Credential::TokenHelper,
            "/opt/relay/litellm-relay",
        )
        .unwrap();
        let doc = parse(&rendered);

        assert_eq!(doc["approval_policy"].as_str(), Some("on-request"));
        assert_eq!(
            doc["model_providers"]["other"]["base_url"].as_str(),
            Some("https://other.example.com/v1")
        );
        assert_eq!(doc["model_provider"].as_str(), Some("litellm"));
    }

    #[test]
    fn should_write_to_env_override_path() {
        let dir = env::temp_dir().join(format!("relay-codex-test-{}", std::process::id()));
        let config_path = dir.join("config.toml");
        let _ = fs::remove_dir_all(&dir);
        env::set_var(CODEX_CONFIG_PATH_ENV, &config_path);

        let settings = settings_with_team(None);
        let written = write_codex_config(
            &settings,
            &Credential::TokenHelper,
            "/opt/relay/litellm-relay",
        )
        .unwrap();

        assert_eq!(written, config_path);
        let contents = fs::read_to_string(&config_path).unwrap();
        assert!(contents.contains("https://gateway.example.com/v1"));

        env::remove_var(CODEX_CONFIG_PATH_ENV);
        let _ = fs::remove_dir_all(&dir);
    }
}
