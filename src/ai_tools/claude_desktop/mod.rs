use std::{env, fs, path::PathBuf, process::Command};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};

use crate::config::{load_settings, save_settings, RelaySettings};

/// The Claude Desktop preferences domain (its macOS bundle identifier). Managed
/// preferences and configuration profiles are keyed by this domain.
pub const CLAUDE_DESKTOP_DOMAIN: &str = "com.anthropic.claudefordesktop";

/// Where the managed inference configuration is deployed, which differs by OS.
///
/// On macOS the app honors inference config only from a *managed preferences*
/// source (a security measure so malware can't silently redirect inference):
/// `/Library/Managed Preferences/<domain>.plist`, an XML property list keyed by
/// the app's bundle id. On Linux the same keys live in a root-owned JSON file
/// under `/etc/claude-desktop/`. User defaults and the Linux `/etc` path are
/// ignored on macOS, which is why writing JSON there left the UI empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopTarget {
    MacOs,
    Linux,
}

impl DesktopTarget {
    /// Detect the target for the running host. Overridable via
    /// `RELAY_CLAUDE_DESKTOP_PLATFORM` (`macos`/`linux`) so the writer can be
    /// exercised for either OS in tests regardless of the build target.
    fn detect() -> Self {
        if let Ok(value) = env::var("RELAY_CLAUDE_DESKTOP_PLATFORM") {
            match value.trim().to_ascii_lowercase().as_str() {
                "macos" | "darwin" | "mac" => return DesktopTarget::MacOs,
                "linux" => return DesktopTarget::Linux,
                _ => {}
            }
        }
        if cfg!(target_os = "macos") {
            DesktopTarget::MacOs
        } else {
            DesktopTarget::Linux
        }
    }
}

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

/// Writes the OS-native managed configuration Claude Desktop reads on launch so
/// it routes inference through the Gateway: a managed-preferences plist on
/// macOS (`/Library/Managed Preferences/com.anthropic.claudefordesktop.plist`)
/// or the root-owned JSON file on Linux (`/etc/claude-desktop/`). The app picks
/// up either on launch, switches into gateway mode, and — in SSO mode — prompts
/// the developer to sign in through their browser on first use.
pub fn onboard_desktop(params: OnboardDesktopParams) -> Result<()> {
    let target = DesktopTarget::detect();
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
    let path = write_managed_settings(&document, target)?;
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

/// Emit a macOS configuration profile (`.mobileconfig`) that pushes the same
/// managed inference keys through MDM. This is the fully supported, zero-user
/// deployment path for a fleet: upload it as a Custom Settings profile in Jamf,
/// Intune, or Kandji. It wraps the `com.anthropic.claudefordesktop` domain in a
/// `com.apple.ManagedClient.preferences` payload (a "Forced" managed setting),
/// which lands in `/Library/Managed Preferences/` exactly where the app reads.
pub fn export_desktop_profile() -> Result<String> {
    let settings = load_settings()?;
    if settings.gateway.api_key.as_deref().unwrap_or("").is_empty() {
        bail!(
            "profile export needs a static Gateway key: set gateway.api_key in Relay config or \
             pass --api-key to onboard-claude-desktop first"
        );
    }
    let document = build_managed_settings(&settings, None);
    Ok(build_config_profile(&document))
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

/// Absolute path of the managed configuration file for `target`. The
/// `CLAUDE_DESKTOP_MANAGED_SETTINGS` env var overrides it (used by the tests and
/// available as an escape hatch).
fn managed_settings_path(target: DesktopTarget) -> PathBuf {
    if let Ok(path) = env::var("CLAUDE_DESKTOP_MANAGED_SETTINGS") {
        return PathBuf::from(path);
    }
    match target {
        DesktopTarget::MacOs => PathBuf::from(format!(
            "/Library/Managed Preferences/{CLAUDE_DESKTOP_DOMAIN}.plist"
        )),
        DesktopTarget::Linux => PathBuf::from("/etc/claude-desktop/managed-settings.json"),
    }
}

/// Serialize `document` in the format `target` reads: an XML property list on
/// macOS (managed preferences are always plists) or pretty JSON on Linux.
fn serialize_managed_settings(document: &Map<String, Value>, target: DesktopTarget) -> String {
    match target {
        DesktopTarget::MacOs => plist_document(&Value::Object(document.clone())),
        DesktopTarget::Linux => {
            let json =
                serde_json::to_string_pretty(&Value::Object(document.clone())).unwrap_or_default();
            format!("{json}\n")
        }
    }
}

fn write_managed_settings(document: &Map<String, Value>, target: DesktopTarget) -> Result<PathBuf> {
    let path = managed_settings_path(target);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| managed_write_error(error, parent, target))?;
    }

    let serialized = serialize_managed_settings(document, target);
    fs::write(&path, serialized).map_err(|error| managed_write_error(error, &path, target))?;

    // macOS caches managed preferences in `cfprefsd`; nudge it so the freshly
    // written plist is read without a reboot. Best-effort: the app also reloads
    // on its own launch, so a failure here is not fatal. Skipped when the path
    // is overridden (tests) to avoid touching the host daemon.
    if target == DesktopTarget::MacOs && env::var("CLAUDE_DESKTOP_MANAGED_SETTINGS").is_err() {
        let _ = Command::new("killall").arg("-HUP").arg("cfprefsd").status();
    }
    Ok(path)
}

/// Maps a filesystem error on the managed config path to a concise, actionable
/// message. Permission errors get a short "needs sudo" hint (surfaced verbatim
/// in the autoconfigure summary) instead of the raw "Permission denied".
fn managed_write_error(
    error: std::io::Error,
    path: &std::path::Path,
    target: DesktopTarget,
) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        let dir = match target {
            DesktopTarget::MacOs => "/Library/Managed Preferences",
            DesktopTarget::Linux => "/etc/claude-desktop",
        };
        anyhow!("needs sudo (managed dir {dir} must be root-owned)")
    } else {
        anyhow::Error::new(error).context(format!("failed to write {}", path.display()))
    }
}

/// Render a full XML property list document (with header + DOCTYPE) for `value`.
fn plist_document(value: &Value) -> String {
    let mut body = String::new();
    write_plist_value(value, 1, &mut body);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n{body}</plist>\n"
    )
}

/// Recursively serialize a JSON value as plist XML at the given indent depth.
/// Handles the value shapes the managed document uses: string, bool, integer,
/// array, and nested dict (the OIDC block).
fn write_plist_value(value: &Value, depth: usize, out: &mut String) {
    let pad = "  ".repeat(depth);
    match value {
        Value::String(text) => {
            out.push_str(&format!("{pad}<string>{}</string>\n", xml_escape(text)))
        }
        Value::Bool(flag) => out.push_str(&format!(
            "{pad}{}\n",
            if *flag { "<true/>" } else { "<false/>" }
        )),
        Value::Number(number) => {
            if number.is_f64() && !number.is_i64() && !number.is_u64() {
                out.push_str(&format!("{pad}<real>{number}</real>\n"));
            } else {
                out.push_str(&format!("{pad}<integer>{number}</integer>\n"));
            }
        }
        Value::Array(items) => {
            out.push_str(&format!("{pad}<array>\n"));
            for item in items {
                write_plist_value(item, depth + 1, out);
            }
            out.push_str(&format!("{pad}</array>\n"));
        }
        Value::Object(map) => {
            out.push_str(&format!("{pad}<dict>\n"));
            let inner = "  ".repeat(depth + 1);
            for (key, val) in map {
                out.push_str(&format!("{inner}<key>{}</key>\n", xml_escape(key)));
                write_plist_value(val, depth + 1, out);
            }
            out.push_str(&format!("{pad}</dict>\n"));
        }
        Value::Null => out.push_str(&format!("{pad}<string></string>\n")),
    }
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Build a `.mobileconfig` that force-installs `document` into the Claude
/// Desktop preferences domain via a `com.apple.ManagedClient.preferences`
/// payload. Deployed through MDM it lands in `/Library/Managed Preferences/`,
/// the same source the app reads, with no per-user action.
fn build_config_profile(document: &Map<String, Value>) -> String {
    // Deterministic UUIDs derived from the domain so redeploying the same
    // profile updates in place rather than stacking duplicates.
    let payload_uuid = "5F1E9A00-0000-4000-8000-000000000001";
    let profile_uuid = "5F1E9A00-0000-4000-8000-000000000000";

    let mut forced_setting = Map::new();
    forced_setting.insert(
        "mcx_preference_settings".into(),
        Value::Object(document.clone()),
    );

    let mut domain_block = Map::new();
    domain_block.insert(
        "Forced".into(),
        Value::Array(vec![Value::Object(forced_setting)]),
    );

    let mut inner_payload = Map::new();
    inner_payload.insert(CLAUDE_DESKTOP_DOMAIN.into(), Value::Object(domain_block));

    let mut prefs_payload = Map::new();
    prefs_payload.insert(
        "PayloadType".into(),
        Value::String("com.apple.ManagedClient.preferences".into()),
    );
    prefs_payload.insert(
        "PayloadIdentifier".into(),
        Value::String("ai.litellm.relay.claude-desktop".into()),
    );
    prefs_payload.insert("PayloadUUID".into(), Value::String(payload_uuid.into()));
    prefs_payload.insert("PayloadVersion".into(), json!(1));
    prefs_payload.insert("PayloadEnabled".into(), Value::Bool(true));
    prefs_payload.insert("PayloadContent".into(), Value::Object(inner_payload));

    let mut root = Map::new();
    root.insert(
        "PayloadContent".into(),
        Value::Array(vec![Value::Object(prefs_payload)]),
    );
    root.insert(
        "PayloadDisplayName".into(),
        Value::String("LiteLLM Relay — Claude Desktop Gateway".into()),
    );
    root.insert(
        "PayloadIdentifier".into(),
        Value::String("ai.litellm.relay.claude-desktop".into()),
    );
    root.insert("PayloadRemovalDisallowed".into(), Value::Bool(false));
    root.insert("PayloadType".into(), Value::String("Configuration".into()));
    root.insert("PayloadUUID".into(), Value::String(profile_uuid.into()));
    root.insert("PayloadVersion".into(), json!(1));

    plist_document(&Value::Object(root))
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

    #[test]
    fn should_target_macos_managed_preferences_plist() {
        // No env override: macOS resolves to the managed-preferences plist
        // keyed by the app's bundle id, not the Linux /etc JSON path.
        assert_eq!(
            managed_settings_path(DesktopTarget::MacOs),
            PathBuf::from("/Library/Managed Preferences/com.anthropic.claudefordesktop.plist")
        );
        assert_eq!(
            managed_settings_path(DesktopTarget::Linux),
            PathBuf::from("/etc/claude-desktop/managed-settings.json")
        );
    }

    #[test]
    fn should_serialize_macos_config_as_plist() {
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-4-5");
        let doc = build_managed_settings(&settings, None);
        let plist = serialize_managed_settings(&doc, DesktopTarget::MacOs);

        assert!(plist.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(plist.contains("<!DOCTYPE plist PUBLIC"));
        assert!(plist.contains("<key>inferenceProvider</key>"));
        assert!(plist.contains("<string>gateway</string>"));
        assert!(plist.contains("<key>inferenceGatewayBaseUrl</key>"));
        assert!(plist.contains("<string>https://gw.corp</string>"));
        assert!(plist.contains("<key>inferenceModels</key>"));
        assert!(plist.contains("<array>"));
        assert!(plist.contains("<string>claude-sonnet-4-5</string>"));
        // Linux still gets JSON.
        let json = serialize_managed_settings(&doc, DesktopTarget::Linux);
        assert!(json.trim_start().starts_with('{'));
    }

    #[test]
    fn should_serialize_nested_oidc_and_ports_in_plist() {
        let settings = settings_with("https://gw.corp", Some("sk-secret"), "claude-sonnet-4-5");
        let sso = SsoConfig {
            client_id: "client-123".into(),
            issuer: "https://login.corp/v2.0".into(),
            scopes: None,
            redirect_port: Some(53180),
        };
        let doc = build_managed_settings(&settings, Some(&sso));
        let plist = serialize_managed_settings(&doc, DesktopTarget::MacOs);

        assert!(plist.contains("<key>inferenceGatewayOidc</key>"));
        assert!(plist.contains("<key>clientId</key>"));
        assert!(plist.contains("<string>client-123</string>"));
        assert!(plist.contains("<integer>53180</integer>"));
        assert!(
            !plist.contains("inferenceGatewayApiKey"),
            "SSO plist must not carry a static key"
        );
    }

    #[test]
    fn should_xml_escape_special_characters() {
        let mut doc = Map::new();
        doc.insert(
            "inferenceGatewayBaseUrl".into(),
            Value::String("https://gw.corp/?a=1&b=2<x>".into()),
        );
        let plist = plist_document(&Value::Object(doc));
        assert!(plist.contains("https://gw.corp/?a=1&amp;b=2&lt;x&gt;"));
        assert!(!plist.contains("&b=2<x>"));
    }

    #[test]
    fn should_build_managed_client_preferences_profile() {
        let settings = settings_with("https://gw.corp", Some("sk-test"), "claude-sonnet-4-5");
        let doc = build_managed_settings(&settings, None);
        let profile = build_config_profile(&doc);

        assert!(profile.contains("<string>Configuration</string>"));
        assert!(profile.contains("<string>com.apple.ManagedClient.preferences</string>"));
        assert!(profile.contains("<key>com.anthropic.claudefordesktop</key>"));
        assert!(profile.contains("<key>Forced</key>"));
        assert!(profile.contains("<key>mcx_preference_settings</key>"));
        assert!(profile.contains("<key>inferenceGatewayApiKey</key>"));
        assert!(profile.contains("<string>sk-test</string>"));
    }
}
