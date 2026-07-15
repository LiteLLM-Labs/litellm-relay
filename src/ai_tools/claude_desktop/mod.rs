use std::{env, fs, path::PathBuf};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};

use crate::config::{load_settings, save_settings, RelaySettings};

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
    /// Suppress success output (used by autoconfigure, which prints its own
    /// summary). Standalone `relay onboard-claude-desktop` leaves this false.
    pub quiet: bool,
}

/// Writes `/etc/claude-desktop/managed-settings.json` so Claude Desktop routes
/// inference through the Gateway. The app reads this root-owned file on launch
/// (see the Anthropic "LLM gateway" third-party docs), switches into gateway
/// mode, and — in SSO mode — prompts the developer to sign in through their
/// browser on first use.
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

    let document = build_managed_settings(&settings, sso.as_ref());
    let path = write_managed_settings(&document)?;
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
        println!("Restart Claude Desktop to pick up the managed configuration.");
    }
    Ok(())
}

struct SsoConfig {
    client_id: String,
    issuer: String,
    scopes: Option<String>,
    redirect_port: Option<u16>,
}

/// Builds the top-level JSON object Claude Desktop reads from
/// `/etc/claude-desktop/managed-settings.json`. Keys match the Anthropic
/// third-party configuration reference exactly.
fn build_managed_settings(settings: &RelaySettings, sso: Option<&SsoConfig>) -> Map<String, Value> {
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
    root.insert(
        "inferenceModels".into(),
        Value::Array(vec![Value::String(settings.claude.model.clone())]),
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
    fn should_write_static_gateway_config() {
        let settings = settings_with(
            "http://127.0.0.1:4000",
            Some("sk-test"),
            "claude-sonnet-4-5",
        );
        let doc = build_managed_settings(&settings, None);

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
        assert_eq!(doc["inferenceModels"], json!(["claude-sonnet-4-5"]));
    }

    #[test]
    fn should_write_interactive_sso_config_without_api_key() {
        let settings = settings_with("https://gw.corp", Some("sk-secret"), "claude-sonnet-4-5");
        let sso = SsoConfig {
            client_id: "client-123".into(),
            issuer: "https://login.corp/v2.0".into(),
            scopes: None,
            redirect_port: Some(53180),
        };
        let doc = build_managed_settings(&settings, Some(&sso));

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
}
