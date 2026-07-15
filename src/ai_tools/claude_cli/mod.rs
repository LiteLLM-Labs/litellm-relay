use std::{env, fs, path::PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::{
    ai_tools::token::ensure_token,
    config::{load_settings, save_settings, RelaySettings},
    system::home_dir,
};

/// How Claude Code should obtain the Gateway bearer credential. These are
/// mutually exclusive: a static key lives in the `ANTHROPIC_AUTH_TOKEN` env var,
/// while the token helper is a top-level `apiKeyHelper` command.
#[derive(Debug, Default)]
enum Credential<'a> {
    /// Top-level `apiKeyHelper` running Relay's token helper (default). Claude
    /// fetches a short-lived identity token on demand; no key on the device.
    #[default]
    TokenHelper,
    /// Static gateway key written to `ANTHROPIC_AUTH_TOKEN` for environments
    /// without an IdP.
    StaticKey(&'a str),
}

/// Inputs for wiring Claude Code to route through the Gateway. Supplied by the
/// MDM package (Jamf/Intune) or interactively; any field left unset falls back
/// to the saved Relay config.
#[derive(Debug, Default)]
pub struct OnboardParams {
    pub gateway_url: Option<String>,
    pub authorize_url: Option<String>,
    pub team: Option<String>,
    pub model: Option<String>,
    /// Static gateway key fallback for environments without an IdP.
    pub api_key: Option<String>,
    /// Suppress success output (used by autoconfigure, which prints its own
    /// summary). Standalone `relay onboard` leaves this false.
    pub quiet: bool,
}

/// Writes `~/.claude/settings.json` so `claude` sends requests to the Gateway
/// with the team header. By default the bearer source is Relay's token helper
/// (`apiKeyHelper`) and the developer signs in through their IdP on first use,
/// so no provider key ever touches the device. When a static Gateway key is
/// supplied (or configured with no IdP), it is written to `ANTHROPIC_AUTH_TOKEN`
/// instead.
pub fn onboard(params: OnboardParams) -> Result<()> {
    let mut settings = load_settings()?;
    if let Some(gateway_url) = params.gateway_url {
        settings.gateway.url = gateway_url.trim_end_matches('/').to_string();
    }
    if let Some(authorize_url) = params.authorize_url {
        settings.idp.authorize_url = authorize_url;
    }
    if let Some(model) = params.model {
        settings.claude.model = model;
    }
    if params.team.is_some() {
        settings.claude.team = params.team;
    }

    // A static key resolves from --api-key, or from a saved gateway key when no
    // IdP is configured. A configured IdP is always preferred.
    let static_key = params
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
        .or_else(|| {
            if settings.idp.authorize_url.trim().is_empty() {
                settings
                    .gateway
                    .api_key
                    .as_deref()
                    .filter(|key| !key.trim().is_empty())
            } else {
                None
            }
        });

    let credential = match static_key {
        Some(key) => Credential::StaticKey(key),
        None if !settings.idp.authorize_url.trim().is_empty() => Credential::TokenHelper,
        None => bail!(
            "onboarding requires an IdP authorize URL (--authorize-url or idp.authorize_url) \
             or a static Gateway key (--api-key or gateway.api_key)"
        ),
    };

    let settings_path = write_claude_settings(&settings, &credential)?;
    save_settings(&settings)?;

    if !params.quiet {
        println!("Claude Code is wired to {}", settings.gateway.url);
        if let Some(team) = &settings.claude.team {
            println!("Team header: x-litellm-team: {team}");
        }
        println!("Wrote {}", settings_path.display());
        match &credential {
            Credential::StaticKey(_) => println!("Using a static gateway key."),
            Credential::TokenHelper => {
                println!("Run `claude` and sign in through your browser on first use.");
            }
        }
    }
    Ok(())
}

/// Prints a valid IdP bearer token on stdout for Claude Code's `apiKeyHelper`.
pub fn print_token() -> Result<()> {
    let settings = load_settings()?;
    let token = ensure_token(&settings.idp.authorize_url)?;
    println!("{token}");
    Ok(())
}

fn claude_settings_path() -> PathBuf {
    home_dir().join(".claude").join("settings.json")
}

fn write_claude_settings(settings: &RelaySettings, credential: &Credential) -> Result<PathBuf> {
    let path = claude_settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let root = read_existing_settings(&path)?;
    let root = merge_claude_settings(root, settings, credential)?;

    let serialized = serde_json::to_string_pretty(&Value::Object(root))?;
    fs::write(&path, format!("{serialized}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Merges Relay's managed keys into an existing `settings.json` object,
/// preserving all other keys. The two credential modes are mutually exclusive:
/// StaticKey writes `ANTHROPIC_AUTH_TOKEN` and drops any `apiKeyHelper`, while
/// TokenHelper writes `apiKeyHelper` and drops `ANTHROPIC_AUTH_TOKEN`.
fn merge_claude_settings(
    mut root: Map<String, Value>,
    settings: &RelaySettings,
    credential: &Credential,
) -> Result<Map<String, Value>> {
    let env = env_object(&mut root);
    env.insert(
        "ANTHROPIC_BASE_URL".into(),
        Value::String(settings.gateway.url.clone()),
    );
    env.insert(
        "ANTHROPIC_MODEL".into(),
        Value::String(settings.claude.model.clone()),
    );
    match &settings.claude.team {
        Some(team) => {
            env.insert(
                "ANTHROPIC_CUSTOM_HEADERS".into(),
                Value::String(format!("x-litellm-team: {team}")),
            );
        }
        None => {
            env.remove("ANTHROPIC_CUSTOM_HEADERS");
        }
    }
    match credential {
        Credential::StaticKey(key) => {
            env.insert(
                "ANTHROPIC_AUTH_TOKEN".into(),
                Value::String((*key).to_string()),
            );
        }
        Credential::TokenHelper => {
            env.remove("ANTHROPIC_AUTH_TOKEN");
        }
    }

    match credential {
        Credential::StaticKey(_) => {
            root.remove("apiKeyHelper");
        }
        Credential::TokenHelper => {
            root.insert(
                "apiKeyHelper".into(),
                Value::String(token_helper_command()?),
            );
        }
    }

    Ok(root)
}

fn read_existing_settings(path: &PathBuf) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    match serde_json::from_str::<Value>(&contents) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Ok(Map::new()),
    }
}

fn env_object(root: &mut Map<String, Value>) -> &mut Map<String, Value> {
    if !matches!(root.get("env"), Some(Value::Object(_))) {
        root.insert("env".into(), json!({}));
    }
    root.get_mut("env")
        .and_then(Value::as_object_mut)
        .expect("env was just inserted as an object")
}

fn token_helper_command() -> Result<String> {
    let exe = env::current_exe().context("failed to resolve the Relay executable path")?;
    let exe = exe
        .to_str()
        .ok_or_else(|| anyhow!("Relay executable path is not valid UTF-8"))?;
    Ok(format!("'{}' claude-token", exe.replace('\'', "'\\''")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_insert_managed_env_and_preserve_existing_keys() {
        let mut root =
            serde_json::from_str::<Value>(r#"{"env":{"EXISTING":"keep"},"otherTopLevel":true}"#)
                .unwrap()
                .as_object()
                .unwrap()
                .clone();

        let env = env_object(&mut root);
        env.insert(
            "ANTHROPIC_BASE_URL".into(),
            Value::String("http://gw".into()),
        );

        assert_eq!(root["env"]["EXISTING"], Value::String("keep".into()));
        assert_eq!(
            root["env"]["ANTHROPIC_BASE_URL"],
            Value::String("http://gw".into())
        );
        assert_eq!(root["otherTopLevel"], Value::Bool(true));
    }

    #[test]
    fn should_create_env_object_when_missing_or_wrong_type() {
        let mut root = serde_json::from_str::<Value>(r#"{"env":"not-an-object"}"#)
            .unwrap()
            .as_object()
            .unwrap()
            .clone();

        let env = env_object(&mut root);
        env.insert("K".into(), Value::String("v".into()));

        assert_eq!(root["env"]["K"], Value::String("v".into()));
    }

    #[test]
    fn should_quote_token_helper_command() {
        let command = token_helper_command().unwrap();
        assert!(command.ends_with("' claude-token"));
        assert!(command.starts_with('\''));
    }

    fn settings_with_team(team: Option<&str>) -> RelaySettings {
        let mut settings = RelaySettings::default();
        settings.gateway.url = "https://gateway.example.com".into();
        settings.claude.model = "claude-sonnet-4-5".into();
        settings.claude.team = team.map(str::to_string);
        settings
    }

    #[test]
    fn should_write_static_auth_token_and_drop_api_key_helper() {
        let settings = settings_with_team(Some("engineering"));
        let existing = serde_json::from_str::<Value>(r#"{"apiKeyHelper":"stale","keep":true}"#)
            .unwrap()
            .as_object()
            .unwrap()
            .clone();

        let root =
            merge_claude_settings(existing, &settings, &Credential::StaticKey("sk-static-123"))
                .unwrap();

        assert_eq!(
            root["env"]["ANTHROPIC_BASE_URL"],
            Value::String("https://gateway.example.com".into())
        );
        assert_eq!(
            root["env"]["ANTHROPIC_MODEL"],
            Value::String("claude-sonnet-4-5".into())
        );
        assert_eq!(
            root["env"]["ANTHROPIC_CUSTOM_HEADERS"],
            Value::String("x-litellm-team: engineering".into())
        );
        assert_eq!(
            root["env"]["ANTHROPIC_AUTH_TOKEN"],
            Value::String("sk-static-123".into())
        );
        assert!(
            !root.contains_key("apiKeyHelper"),
            "static key mode must remove any apiKeyHelper so the two do not conflict"
        );
        assert_eq!(root["keep"], Value::Bool(true));
    }

    #[test]
    fn should_write_api_key_helper_and_drop_static_auth_token() {
        let settings = settings_with_team(None);
        let existing =
            serde_json::from_str::<Value>(r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-stale"}}"#)
                .unwrap()
                .as_object()
                .unwrap()
                .clone();

        let root = merge_claude_settings(existing, &settings, &Credential::TokenHelper).unwrap();

        assert!(
            root["apiKeyHelper"]
                .as_str()
                .unwrap()
                .ends_with("claude-token"),
            "token helper mode must set apiKeyHelper"
        );
        assert!(
            root["env"]
                .as_object()
                .unwrap()
                .get("ANTHROPIC_AUTH_TOKEN")
                .is_none(),
            "token helper mode must remove a stale static ANTHROPIC_AUTH_TOKEN"
        );
        assert!(
            !root["env"]
                .as_object()
                .unwrap()
                .contains_key("ANTHROPIC_CUSTOM_HEADERS"),
            "no team means no custom headers"
        );
    }
}
