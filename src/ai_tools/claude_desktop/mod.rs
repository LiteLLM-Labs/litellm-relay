use std::{env, fs, path::PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::{
    config::{load_settings, save_settings, RelaySettings},
    system::home_dir,
};

/// Inputs for wiring Claude Desktop (third-party mode) to route through the
/// Gateway. Supplied by the MDM package or interactively; any field left unset
/// falls back to the saved Relay config.
///
/// When `oidc_client_id` and `oidc_issuer` are both set, the app is configured
/// for single sign-on: each developer signs in against the corporate IdP and
/// the resulting token is sent to the Gateway as the bearer credential, so no
/// provider key ever lands on the device. Otherwise a static Gateway key is
/// written.
#[derive(Debug, Default)]
pub struct OnboardDesktopParams {
    pub gateway_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_issuer: Option<String>,
    pub oidc_scopes: Option<String>,
    pub oidc_redirect_port: Option<u16>,
    /// Write the fleet-managed, root-owned settings file
    /// (`/etc/claude-desktop/managed-settings.json`, for MDM deploys) instead
    /// of the per-user config. Requires root. Defaults to false — on macOS the
    /// default is the user-writable `Claude-3p/configLibrary` path (no sudo).
    pub managed: bool,
    /// Suppress success output (used by autoconfigure, which prints its own
    /// summary). Standalone `relay onboard-claude-desktop` leaves this false.
    pub quiet: bool,
}

/// Configures Claude Desktop to route inference through the Gateway.
///
/// By default (personal machine) this writes the **user-writable** local
/// config that the app's "Apply locally" action uses — no root required:
///   `~/Library/Application Support/Claude-3p/configLibrary/<appliedId>.json`
/// plus `_meta.json` (which records the applied config). Claude Desktop reads
/// it on launch and switches into gateway mode.
///
/// With `managed: true` (MDM / fleet deploy) it writes the root-owned
/// `/etc/claude-desktop/managed-settings.json` enforced-settings file instead.
pub fn onboard_desktop(params: OnboardDesktopParams) -> Result<()> {
    let mut settings = load_settings()?;
    if let Some(gateway_url) = params.gateway_url {
        settings.gateway.url = gateway_url.trim_end_matches('/').to_string();
    }
    if let Some(api_key) = params.api_key {
        settings.gateway.api_key = Some(api_key);
    }
    if let Some(model) = params.model {
        settings.claude.model = model;
    }

    let sso = match (&params.oidc_client_id, &params.oidc_issuer) {
        (Some(client_id), Some(issuer)) => Some(SsoConfig {
            client_id: client_id.clone(),
            issuer: issuer.clone(),
            scopes: params.oidc_scopes.clone(),
            redirect_port: params.oidc_redirect_port,
        }),
        (None, None) => None,
        _ => bail!("SSO requires both --oidc-client-id and --oidc-issuer"),
    };

    if sso.is_none() && settings.gateway.api_key.as_deref().unwrap_or("").is_empty() {
        bail!(
            "Claude Desktop onboarding needs a Gateway credential: pass --api-key for a static \
             key, or --oidc-client-id and --oidc-issuer for single sign-on"
        );
    }

    let document = build_inference_settings(&settings, sso.as_ref());

    // Default to the per-user config on macOS (no sudo). Use the root-owned
    // managed file only when explicitly requested, or on other platforms.
    let use_managed = params.managed || !cfg!(target_os = "macos");
    let path = if use_managed {
        write_managed_settings(&document)?
    } else {
        write_config_library(&document)?
    };
    save_settings(&settings)?;

    if !params.quiet {
        println!("Claude Desktop is wired to {}", settings.gateway.url);
        match &sso {
            Some(sso) => {
                println!(
                    "Sign-in: OIDC issuer {} (client {})",
                    sso.issuer, sso.client_id
                );
                println!("Developers click \"Sign in to your organization\" on first launch.");
            }
            None => println!("Credential: static Gateway API key"),
        }
        println!("Wrote {}", path.display());
        println!("Restart Claude Desktop to pick up the configuration.");
    }
    Ok(())
}

struct SsoConfig {
    client_id: String,
    issuer: String,
    scopes: Option<String>,
    redirect_port: Option<u16>,
}

/// Builds the inference-config object Claude Desktop reads (same keys for the
/// per-user `configLibrary` entry and the managed-settings file). Keys match
/// the Anthropic third-party configuration reference.
fn build_inference_settings(
    settings: &RelaySettings,
    sso: Option<&SsoConfig>,
) -> Map<String, Value> {
    let mut root = Map::new();
    root.insert("inferenceProvider".into(), Value::String("gateway".into()));
    root.insert(
        "inferenceGatewayBaseUrl".into(),
        Value::String(settings.gateway.url.clone()),
    );
    root.insert(
        "inferenceGatewayAuthScheme".into(),
        Value::String("bearer".into()),
    );
    root.insert("modelDiscoveryEnabled".into(), Value::Bool(true));
    // Model entries are objects keyed by `name` (the exact id `/v1/models` returns).
    root.insert(
        "inferenceModels".into(),
        json!([{ "name": settings.claude.model.clone() }]),
    );

    match sso {
        Some(sso) => {
            root.insert(
                "inferenceCredentialKind".into(),
                Value::String("interactive".into()),
            );
            let mut oidc = Map::new();
            oidc.insert("clientId".into(), Value::String(sso.client_id.clone()));
            oidc.insert("issuer".into(), Value::String(sso.issuer.clone()));
            if let Some(scopes) = &sso.scopes {
                oidc.insert("scopes".into(), Value::String(scopes.clone()));
            }
            if let Some(port) = sso.redirect_port {
                oidc.insert("redirectPort".into(), json!(port));
            }
            root.insert("inferenceGatewayOidc".into(), Value::Object(oidc));
        }
        None => {
            root.insert(
                "inferenceCredentialKind".into(),
                Value::String("static".into()),
            );
            if let Some(api_key) = &settings.gateway.api_key {
                root.insert(
                    "inferenceGatewayApiKey".into(),
                    Value::String(api_key.clone()),
                );
            }
        }
    }

    root
}

// MARK: - Per-user config (macOS, no sudo)

fn config_library_dir() -> PathBuf {
    if let Ok(path) = env::var("CLAUDE_3P_CONFIG_LIBRARY") {
        return PathBuf::from(path);
    }
    home_dir()
        .join("Library")
        .join("Application Support")
        .join("Claude-3p")
        .join("configLibrary")
}

/// Writes the per-user config that Claude Desktop's "Apply locally" uses:
/// `configLibrary/<appliedId>.json` (the inference settings) plus `_meta.json`
/// (which records the applied config id and the entry list). Reuses the
/// existing applied id when present (merging into that entry), otherwise
/// creates a new "Default" entry and marks it applied. No root required.
fn write_config_library(inference: &Map<String, Value>) -> Result<PathBuf> {
    write_config_library_in(&config_library_dir(), inference)
}

/// Testable core of [`write_config_library`] with the target directory injected
/// (so tests don't race on the `CLAUDE_3P_CONFIG_LIBRARY` env var).
fn write_config_library_in(
    dir: &std::path::Path,
    inference: &Map<String, Value>,
) -> Result<PathBuf> {
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;

    let meta_path = dir.join("_meta.json");
    let mut meta = read_json_object(&meta_path);

    let applied_id = meta
        .get("appliedId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    // Merge the inference settings into the applied config entry, preserving
    // any unrelated keys already there.
    let config_path = dir.join(format!("{applied_id}.json"));
    let mut config = read_json_object(&config_path);
    for (key, value) in inference {
        config.insert(key.clone(), value.clone());
    }
    write_json(&config_path, &config)?;

    // Ensure _meta.json points at this entry and lists it.
    let mut entries = meta
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !entries
        .iter()
        .any(|entry| entry.get("id").and_then(Value::as_str) == Some(applied_id.as_str()))
    {
        entries.push(json!({ "id": applied_id, "name": "Default" }));
    }
    meta.insert("appliedId".into(), Value::String(applied_id.clone()));
    meta.insert("entries".into(), Value::Array(entries));
    write_json(&meta_path, &meta)?;

    Ok(config_path)
}

fn read_json_object(path: &std::path::Path) -> Map<String, Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn write_json(path: &std::path::Path, object: &Map<String, Value>) -> Result<()> {
    let serialized = serde_json::to_string_pretty(&Value::Object(object.clone()))?;
    fs::write(path, format!("{serialized}\n"))
        .with_context(|| format!("failed to write {}", path.display()))
}

// MARK: - Managed settings (MDM / non-macOS, root-owned)

fn managed_settings_path() -> PathBuf {
    if let Ok(path) = env::var("CLAUDE_DESKTOP_MANAGED_SETTINGS") {
        return PathBuf::from(path);
    }
    PathBuf::from("/etc/claude-desktop/managed-settings.json")
}

fn write_managed_settings(document: &Map<String, Value>) -> Result<PathBuf> {
    let path = managed_settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| managed_write_error(error, parent))?;
    }

    let serialized = serde_json::to_string_pretty(&Value::Object(document.clone()))?;
    fs::write(&path, format!("{serialized}\n"))
        .map_err(|error| managed_write_error(error, &path))?;
    Ok(path)
}

/// Maps a filesystem error on the managed `/etc/claude-desktop` path to a
/// concise, actionable message. Permission errors get a short "needs sudo" hint
/// (surfaced verbatim in the autoconfigure summary) instead of the raw
/// "Permission denied (os error 13)".
fn managed_write_error(error: std::io::Error, path: &std::path::Path) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        anyhow!("needs sudo (managed dir /etc/claude-desktop must be root-owned)")
    } else {
        anyhow::Error::new(error).context(format!("failed to write {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(url: &str, key: Option<&str>, model: &str) -> RelaySettings {
        let mut settings = RelaySettings::default();
        settings.gateway.url = url.into();
        settings.gateway.api_key = key.map(str::to_string);
        settings.claude.model = model.into();
        settings
    }

    #[test]
    fn should_build_static_gateway_config() {
        let settings = settings_with(
            "http://127.0.0.1:4000",
            Some("sk-test"),
            "claude-sonnet-4-5",
        );
        let doc = build_inference_settings(&settings, None);

        assert_eq!(doc["inferenceProvider"], Value::String("gateway".into()));
        assert_eq!(
            doc["inferenceGatewayBaseUrl"],
            Value::String("http://127.0.0.1:4000".into())
        );
        assert_eq!(
            doc["inferenceCredentialKind"],
            Value::String("static".into())
        );
        assert_eq!(
            doc["inferenceGatewayApiKey"],
            Value::String("sk-test".into())
        );
        assert!(!doc.contains_key("inferenceGatewayOidc"));
        assert_eq!(doc["modelDiscoveryEnabled"], Value::Bool(true));
        assert_eq!(
            doc["inferenceModels"],
            json!([{ "name": "claude-sonnet-4-5" }])
        );
    }

    #[test]
    fn should_build_interactive_sso_config_without_api_key() {
        let settings = settings_with("https://gw.corp", Some("sk-secret"), "claude-sonnet-4-5");
        let sso = SsoConfig {
            client_id: "client-123".into(),
            issuer: "https://login.corp/v2.0".into(),
            scopes: None,
            redirect_port: Some(53180),
        };
        let doc = build_inference_settings(&settings, Some(&sso));

        assert_eq!(
            doc["inferenceCredentialKind"],
            Value::String("interactive".into())
        );
        assert!(
            !doc.contains_key("inferenceGatewayApiKey"),
            "SSO mode must not leak a static key onto the device"
        );
        let oidc = doc["inferenceGatewayOidc"].as_object().unwrap();
        assert_eq!(oidc["clientId"], Value::String("client-123".into()));
        assert_eq!(
            oidc["issuer"],
            Value::String("https://login.corp/v2.0".into())
        );
        assert_eq!(oidc["redirectPort"], json!(53180));
    }

    #[test]
    fn should_write_user_config_library_and_meta() {
        let dir = env::temp_dir().join(format!("relay-cfglib-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);

        let settings = settings_with("https://gw.example", Some("sk-abc"), "claude-x");
        let doc = build_inference_settings(&settings, None);
        let written = write_config_library_in(&dir, &doc).unwrap();

        let meta = read_json_object(&dir.join("_meta.json"));
        let applied = meta["appliedId"].as_str().unwrap().to_string();
        assert!(!applied.is_empty());
        assert_eq!(written, dir.join(format!("{applied}.json")));
        assert!(meta["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"].as_str() == Some(applied.as_str())));

        let config = read_json_object(&written);
        assert_eq!(config["inferenceProvider"], Value::String("gateway".into()));
        assert_eq!(
            config["inferenceGatewayBaseUrl"],
            Value::String("https://gw.example".into())
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_reuse_existing_applied_id_and_preserve_keys() {
        let dir = env::temp_dir().join(format!("relay-cfglib-reuse-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("_meta.json"),
            r#"{"appliedId":"abc-123","entries":[{"id":"abc-123","name":"Default"}]}"#,
        )
        .unwrap();
        fs::write(dir.join("abc-123.json"), r#"{"keepMe":true}"#).unwrap();

        let settings = settings_with("https://gw.example", Some("sk-abc"), "claude-x");
        let doc = build_inference_settings(&settings, None);
        let written = write_config_library_in(&dir, &doc).unwrap();

        assert_eq!(written, dir.join("abc-123.json"));
        let config = read_json_object(&written);
        assert_eq!(
            config["keepMe"],
            Value::Bool(true),
            "must preserve existing keys"
        );
        assert_eq!(config["inferenceProvider"], Value::String("gateway".into()));

        let _ = fs::remove_dir_all(&dir);
    }
}
