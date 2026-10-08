use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::apps::{default_ai_domains, default_notion_domains, domain_matches_host};
use crate::{broker::caller::AllowedCaller, system::home_dir};

#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub host: String,
    pub port: u16,
    pub log_path: PathBuf,
    pub notion_domains: Vec<String>,
    pub ai_domains: Vec<String>,
    pub shadow_enabled: bool,
    pub gateway_url: String,
    pub gateway_api_key: Option<String>,
    pub gateway_enrolled_at: Option<DateTime<Utc>>,
    pub gateway_expires_at: Option<DateTime<Utc>>,
    pub shadow_model: String,
    pub shadow_min_interval_seconds: u64,
    pub request_timeout_seconds: f64,
    pub payload_preview_bytes: usize,
    pub payload_body_bytes: usize,
    pub mitm_enabled: bool,
    pub mitm_ca_dir: PathBuf,
}

impl RelayConfig {
    pub fn load() -> Result<Self> {
        Ok(load_settings()?.to_config())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RelaySettings {
    pub relay: RelaySection,
    pub gateway: GatewaySection,
    pub shadow: ShadowSection,
    pub capture: CaptureSection,
    pub domains: DomainSection,
    pub timeouts: TimeoutSection,
    pub idp: IdpSection,
    pub claude: ClaudeSection,
    pub codex: CodexSection,
    pub credential: CredentialSection,
    pub mcp: McpSection,
}

impl RelaySettings {
    pub fn to_config(&self) -> RelayConfig {
        RelayConfig {
            host: self.relay.host.clone(),
            port: self.relay.port,
            log_path: self.relay.log_path.clone(),
            notion_domains: self.domains.notion.clone(),
            ai_domains: self.domains.ai.clone(),
            shadow_enabled: self.shadow.enabled,
            gateway_url: self.gateway.url.clone(),
            gateway_api_key: self
                .gateway
                .api_key
                .clone()
                .filter(|value| !value.is_empty()),
            gateway_enrolled_at: self.gateway.enrolled_at,
            gateway_expires_at: self.gateway.expires_at,
            shadow_model: self.shadow.model.clone(),
            shadow_min_interval_seconds: self.shadow.min_interval_seconds,
            request_timeout_seconds: self.timeouts.request_seconds,
            payload_preview_bytes: self.capture.payload_preview_bytes,
            payload_body_bytes: self.capture.payload_body_bytes,
            mitm_enabled: self.capture.payloads,
            mitm_ca_dir: self.relay.mitm_ca_dir.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct RelaySection {
    pub host: String,
    pub port: u16,
    pub log_path: PathBuf,
    pub mitm_ca_dir: PathBuf,
}

impl Default for RelaySection {
    fn default() -> Self {
        let relay_home = relay_home();
        Self {
            host: "127.0.0.1".into(),
            port: 4142,
            log_path: relay_home.join("relay.log.jsonl"),
            mitm_ca_dir: relay_home.join("mitm"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct GatewaySection {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enrolled_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
}

impl Default for GatewaySection {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:4000".into(),
            api_key: None,
            enrolled_at: None,
            expires_at: None,
        }
    }
}

impl GatewaySection {
    pub fn enroll(&mut self, api_key: String, expires_at: Option<DateTime<Utc>>) {
        self.api_key = Some(api_key);
        self.enrolled_at = Some(Utc::now());
        self.expires_at = expires_at;
    }

    pub fn enroll_if_changed(&mut self, api_key: String) {
        if self.api_key.as_deref() == Some(api_key.as_str()) {
            return;
        }
        self.enroll(api_key, None);
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ShadowSection {
    pub enabled: bool,
    pub model: String,
    pub min_interval_seconds: u64,
}

impl Default for ShadowSection {
    fn default() -> Self {
        Self {
            enabled: false,
            model: "gpt-4o-mini".into(),
            min_interval_seconds: 60,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct CaptureSection {
    pub payloads: bool,
    pub payload_preview_bytes: usize,
    pub payload_body_bytes: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct DomainSection {
    pub notion: Vec<String>,
    pub ai: Vec<String>,
}

impl Default for DomainSection {
    fn default() -> Self {
        Self {
            notion: default_notion_domains(),
            ai: default_ai_domains(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct TimeoutSection {
    pub request_seconds: f64,
}

impl Default for TimeoutSection {
    fn default() -> Self {
        Self {
            request_seconds: 10.0,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct IdpSection {
    pub issuer: String,
    pub client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_port: Option<u16>,
    #[serde(skip_serializing)]
    pub authorize_url: String,
}

impl IdpSection {
    pub fn is_configured(&self) -> bool {
        !self.normalized_issuer().is_empty() && !self.client_id.trim().is_empty()
    }

    pub fn normalized_issuer(&self) -> &str {
        self.issuer.trim().trim_end_matches('/')
    }

    pub fn setup_hint(&self) -> &'static str {
        if self.authorize_url.trim().is_empty() {
            "set --oidc-issuer and --oidc-client-id, or idp.issuer and idp.client_id in config.yaml"
        } else {
            "idp.authorize_url is no longer used: Relay signs in with OIDC authorization code plus \
             PKCE, so set --oidc-issuer and --oidc-client-id instead"
        }
    }

    pub fn apply(&mut self, overrides: &IdpOverrides) {
        if let Some(issuer) = &overrides.issuer {
            self.issuer = issuer.trim().trim_end_matches('/').to_string();
        }
        if let Some(client_id) = &overrides.client_id {
            self.client_id = client_id.trim().to_string();
        }
        if overrides.scopes.is_some() {
            self.scopes = overrides.scopes.clone();
        }
        if overrides.redirect_port.is_some() {
            self.redirect_port = overrides.redirect_port;
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IdpOverrides {
    pub issuer: Option<String>,
    pub client_id: Option<String>,
    pub scopes: Option<String>,
    pub redirect_port: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ClaudeSection {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_sso: Option<DesktopSso>,
}

impl Default for ClaudeSection {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-5".into(),
            team: None,
            desktop_sso: None,
        }
    }
}

/// OIDC settings Claude Desktop was enrolled with, kept so unattended
/// autoconfigure reruns can rebuild the same single sign-on document.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct DesktopSso {
    pub client_id: String,
    pub issuer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_port: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct CodexSection {
    pub model: String,
    pub provider_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

impl Default for CodexSection {
    fn default() -> Self {
        Self {
            model: "gpt-5-codex".into(),
            provider_id: "litellm".into(),
            team: None,
        }
    }
}

/// The credential broker's managed settings. `allowed_callers` replaces the
/// built-in list of client signatures the broker serves when present.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct CredentialSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_callers: Option<Vec<AllowedCaller>>,
}

pub const DEFAULT_MCP_MAX_CONCURRENT_CALLS: usize = 16;
pub const DEFAULT_MCP_TOOL_PREFIX_SEPARATOR: &str = "-";

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct McpSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent_calls: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_prefix_separator: Option<String>,
}

impl McpSection {
    pub fn allowed_tools(&self) -> &[String] {
        self.allow.as_deref().unwrap_or_default()
    }

    pub fn concurrent_call_cap(&self) -> usize {
        match self.max_concurrent_calls {
            None | Some(0) => DEFAULT_MCP_MAX_CONCURRENT_CALLS,
            Some(cap) => cap,
        }
    }

    pub fn tool_prefix_separator(&self) -> &str {
        match self.tool_prefix_separator.as_deref() {
            None | Some("") => DEFAULT_MCP_TOOL_PREFIX_SEPARATOR,
            Some(separator) => separator,
        }
    }
}

pub fn relay_home() -> PathBuf {
    home_dir().join(".litellm-relay")
}

pub fn config_path() -> PathBuf {
    relay_home().join("config.yaml")
}

pub fn load_settings() -> Result<RelaySettings> {
    let path = config_path();
    if path.exists() {
        let contents = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        return serde_yaml::from_str(&contents)
            .with_context(|| format!("failed to parse {}", path.display()));
    }

    if let Some(settings) = load_legacy_env_settings()? {
        save_settings(&settings)?;
        return Ok(settings);
    }

    Ok(RelaySettings::default())
}

pub fn save_settings(settings: &RelaySettings) -> Result<PathBuf> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let contents = serde_yaml::to_string(settings).context("failed to serialize Relay config")?;
    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    Ok(path)
}

pub fn normalize_host(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest).to_string();
    }
    host.split(':').next().unwrap_or(&host).to_string()
}

pub fn is_ai_host(host: &str, config: &RelayConfig) -> bool {
    domain_matches_host(host, &config.ai_domains)
}

pub fn is_notion_host(host: &str, config: &RelayConfig) -> bool {
    domain_matches_host(host, &config.notion_domains)
}

fn load_legacy_env_settings() -> Result<Option<RelaySettings>> {
    let path = relay_home().join("env");
    if !path.exists() {
        return Ok(None);
    }

    let contents =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let values = parse_legacy_env(&contents);
    let mut settings = RelaySettings::default();

    if let Some(value) = values.get("LITELLM_RELAY_HOST") {
        settings.relay.host = value.clone();
    }
    if let Some(value) = parse_legacy_value::<u16>(&values, "LITELLM_RELAY_PORT") {
        settings.relay.port = value;
    }
    if let Some(value) = values.get("LITELLM_RELAY_LOG_PATH") {
        settings.relay.log_path = PathBuf::from(value);
    }
    if let Some(value) = values.get("LITELLM_RELAY_MITM_CA_DIR") {
        settings.relay.mitm_ca_dir = PathBuf::from(value);
    }
    if let Some(value) = values.get("LITELLM_GATEWAY_URL") {
        settings.gateway.url = value.clone();
    }
    settings.gateway.api_key = values
        .get("LITELLM_GATEWAY_API_KEY")
        .or_else(|| values.get("LITELLM_API_KEY"))
        .filter(|value| !value.is_empty())
        .cloned();
    if let Some(value) = parse_legacy_bool(&values, "LITELLM_RELAY_SHADOW_ENABLED") {
        settings.shadow.enabled = value;
    }
    if let Some(value) = values.get("LITELLM_RELAY_SHADOW_MODEL") {
        settings.shadow.model = value.clone();
    }
    if let Some(value) =
        parse_legacy_value::<u64>(&values, "LITELLM_RELAY_SHADOW_MIN_INTERVAL_SECONDS")
    {
        settings.shadow.min_interval_seconds = value;
    }
    if let Some(value) = parse_legacy_bool(&values, "LITELLM_RELAY_CAPTURE_PAYLOADS") {
        settings.capture.payloads = value;
    }
    if let Some(value) = parse_legacy_value::<usize>(&values, "LITELLM_RELAY_PAYLOAD_PREVIEW_BYTES")
    {
        settings.capture.payload_preview_bytes = value;
    }
    if let Some(value) = parse_legacy_value::<usize>(&values, "LITELLM_RELAY_PAYLOAD_BODY_BYTES") {
        settings.capture.payload_body_bytes = value;
    }
    if let Some(value) = parse_legacy_value::<f64>(&values, "LITELLM_RELAY_REQUEST_TIMEOUT_SECONDS")
    {
        settings.timeouts.request_seconds = value;
    }
    if let Some(value) = values.get("LITELLM_RELAY_NOTION_DOMAINS") {
        settings.domains.notion = parse_domains(value);
    }
    if let Some(value) = values.get("LITELLM_RELAY_AI_DOMAINS") {
        settings.domains.ai = parse_domains(value);
    }

    Ok(Some(settings))
}

fn parse_legacy_env(contents: &str) -> HashMap<String, String> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
        .filter(|(key, _)| !key.is_empty())
        .collect()
}

fn parse_legacy_bool(values: &HashMap<String, String>, key: &str) -> Option<bool> {
    values.get(key).map(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn parse_legacy_value<T>(values: &HashMap<String, String>, key: &str) -> Option<T>
where
    T: std::str::FromStr,
{
    values.get(key).and_then(|value| value.parse().ok())
}

fn parse_domains(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_load_partial_yaml_with_defaults() {
        let settings: RelaySettings = serde_yaml::from_str(
            r#"
gateway:
  url: https://gateway.example.com
relay:
  port: 4143
capture:
  payloads: false
"#,
        )
        .expect("settings yaml should parse");

        let config = settings.to_config();

        assert_eq!(config.gateway_url, "https://gateway.example.com");
        assert_eq!(config.port, 4143);
        assert!(!config.mitm_enabled);
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.shadow_model, "gpt-4o-mini");
        assert!(!config.ai_domains.is_empty());
    }

    #[test]
    fn should_read_the_mcp_allow_list_call_cap_and_separator_from_yaml() {
        let settings: RelaySettings = serde_yaml::from_str(
            "mcp:\n  allow:\n    - github-get_issue\n    - Jira-searchIssues\n  max_concurrent_calls: 4\n  tool_prefix_separator: \"__\"\n",
        )
        .expect("settings");
        assert_eq!(
            settings.mcp.allowed_tools(),
            [
                "github-get_issue".to_string(),
                "Jira-searchIssues".to_string()
            ]
        );
        assert_eq!(settings.mcp.concurrent_call_cap(), 4);
        assert_eq!(settings.mcp.tool_prefix_separator(), "__");
    }

    #[test]
    fn should_default_the_mcp_call_cap_and_separator_when_missing_zero_or_empty() {
        let missing: RelaySettings =
            serde_yaml::from_str("gateway:\n  url: https://g\n").expect("settings");
        let zero: RelaySettings = serde_yaml::from_str(
            "mcp:\n  max_concurrent_calls: 0\n  tool_prefix_separator: \"\"\n",
        )
        .expect("settings");
        assert!(missing.mcp.allowed_tools().is_empty());
        assert_eq!(missing.mcp.concurrent_call_cap(), 16);
        assert_eq!(missing.mcp.tool_prefix_separator(), "-");
        assert_eq!(zero.mcp.concurrent_call_cap(), 16);
        assert_eq!(zero.mcp.tool_prefix_separator(), "-");
    }

    #[test]
    fn should_load_gateway_section_without_credential_timestamps() {
        let settings: RelaySettings = serde_yaml::from_str(
            r#"
gateway:
  url: https://gateway.example.com
  api_key: sk-test
"#,
        )
        .expect("settings yaml should parse");

        assert_eq!(settings.gateway.enrolled_at, None);
        assert_eq!(settings.gateway.expires_at, None);
        let config = settings.to_config();
        assert_eq!(config.gateway_enrolled_at, None);
        assert_eq!(config.gateway_expires_at, None);
    }

    #[test]
    fn should_round_trip_credential_timestamps() {
        let enrolled_at = DateTime::parse_from_rfc3339("2026-09-21T21:27:58Z")
            .unwrap()
            .with_timezone(&Utc);
        let expires_at = DateTime::parse_from_rfc3339("2026-09-22T21:27:58.096Z")
            .unwrap()
            .with_timezone(&Utc);
        let settings = RelaySettings {
            gateway: GatewaySection {
                url: "https://gateway.example.com".into(),
                api_key: Some("sk-test".into()),
                enrolled_at: Some(enrolled_at),
                expires_at: Some(expires_at),
            },
            ..RelaySettings::default()
        };

        let yaml = serde_yaml::to_string(&settings).unwrap();
        let reloaded: RelaySettings = serde_yaml::from_str(&yaml).unwrap();

        assert_eq!(reloaded.gateway.enrolled_at, Some(enrolled_at));
        assert_eq!(reloaded.gateway.expires_at, Some(expires_at));
        assert_eq!(reloaded.to_config().gateway_expires_at, Some(expires_at));
    }

    #[test]
    fn should_stamp_enrollment_when_a_new_key_replaces_the_saved_one() {
        let stale = DateTime::parse_from_rfc3339("2026-09-21T21:27:58Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut gateway = GatewaySection {
            api_key: Some("sk-old".into()),
            enrolled_at: Some(stale),
            expires_at: Some(stale),
            ..GatewaySection::default()
        };
        let before = Utc::now();

        gateway.enroll("sk-new".into(), None);

        assert_eq!(gateway.api_key.as_deref(), Some("sk-new"));
        assert!(gateway.enrolled_at.is_some_and(|at| at >= before));
        assert_eq!(gateway.expires_at, None);
    }

    #[test]
    fn should_keep_enrollment_timestamps_when_the_saved_key_is_handed_back() {
        let enrolled_at = DateTime::parse_from_rfc3339("2026-09-21T21:27:58Z")
            .unwrap()
            .with_timezone(&Utc);
        let expires_at = DateTime::parse_from_rfc3339("2026-09-22T21:27:58Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut gateway = GatewaySection {
            api_key: Some("sk-saved".into()),
            enrolled_at: Some(enrolled_at),
            expires_at: Some(expires_at),
            ..GatewaySection::default()
        };

        gateway.enroll_if_changed("sk-saved".into());

        assert_eq!(gateway.enrolled_at, Some(enrolled_at));
        assert_eq!(gateway.expires_at, Some(expires_at));

        gateway.enroll_if_changed("sk-new".into());

        assert_eq!(gateway.api_key.as_deref(), Some("sk-new"));
        assert!(gateway.enrolled_at.is_some_and(|at| at > enrolled_at));
        assert_eq!(gateway.expires_at, None);
    }

    #[test]
    fn should_omit_unset_credential_timestamps_from_yaml() {
        let yaml = serde_yaml::to_string(&RelaySettings::default()).unwrap();

        assert!(!yaml.contains("enrolled_at"), "{yaml}");
        assert!(!yaml.contains("expires_at"), "{yaml}");
    }

    #[test]
    fn should_default_to_metadata_only_capture() {
        let config = RelaySettings::default().to_config();

        assert!(!config.mitm_enabled);
        assert_eq!(config.payload_preview_bytes, 0);
        assert_eq!(config.payload_body_bytes, 0);
    }

    #[test]
    fn should_preserve_explicit_capture_limits() {
        let settings: RelaySettings = serde_yaml::from_str(
            r#"
capture:
  payloads: true
  payload_preview_bytes: 8192
  payload_body_bytes: 262144
"#,
        )
        .expect("settings yaml should parse");
        let config = settings.to_config();

        assert!(config.mitm_enabled);
        assert_eq!(config.payload_preview_bytes, 8192);
        assert_eq!(config.payload_body_bytes, 262_144);
    }

    #[test]
    fn should_migrate_legacy_env_values_without_process_env() {
        let values = parse_legacy_env(
            "LITELLM_RELAY_PORT=4144\n\
             LITELLM_GATEWAY_URL=https://gateway.example.com\n\
             LITELLM_RELAY_CAPTURE_PAYLOADS=0\n",
        );

        assert_eq!(
            parse_legacy_value::<u16>(&values, "LITELLM_RELAY_PORT"),
            Some(4144)
        );
        assert_eq!(
            parse_legacy_bool(&values, "LITELLM_RELAY_CAPTURE_PAYLOADS"),
            Some(false)
        );
        assert_eq!(
            values.get("LITELLM_GATEWAY_URL").map(String::as_str),
            Some("https://gateway.example.com")
        );
    }

    fn idp(issuer: &str, client_id: &str) -> IdpSection {
        IdpSection {
            issuer: issuer.into(),
            client_id: client_id.into(),
            ..IdpSection::default()
        }
    }

    #[test]
    fn should_need_both_issuer_and_client_id_to_be_configured() {
        assert!(idp("https://login.example.com", "client-1").is_configured());
        assert!(!idp("https://login.example.com", " ").is_configured());
        assert!(!idp(" / ", "client-1").is_configured());
        assert!(!IdpSection::default().is_configured());
    }

    #[test]
    fn should_normalize_the_issuer_the_same_way_everywhere() {
        assert_eq!(
            idp(" https://login.example.com/tenant/v2.0/ ", "client-1").normalized_issuer(),
            "https://login.example.com/tenant/v2.0"
        );
    }

    #[test]
    fn should_apply_only_the_overrides_that_were_passed() {
        let mut section = IdpSection {
            scopes: Some("openid".into()),
            redirect_port: Some(53180),
            ..idp("https://old.example.com", "old-client")
        };

        section.apply(&IdpOverrides {
            issuer: Some(" https://login.example.com/ ".into()),
            client_id: Some(" new-client ".into()),
            ..IdpOverrides::default()
        });

        assert_eq!(section.issuer, "https://login.example.com");
        assert_eq!(section.client_id, "new-client");
        assert_eq!(section.scopes.as_deref(), Some("openid"));
        assert_eq!(section.redirect_port, Some(53180));

        section.apply(&IdpOverrides {
            scopes: Some("openid groups".into()),
            redirect_port: Some(53999),
            ..IdpOverrides::default()
        });

        assert_eq!(section.issuer, "https://login.example.com");
        assert_eq!(section.client_id, "new-client");
        assert_eq!(section.scopes.as_deref(), Some("openid groups"));
        assert_eq!(section.redirect_port, Some(53999));
    }

    #[test]
    fn should_point_a_legacy_authorize_url_config_at_the_oidc_flags() {
        let legacy: RelaySettings =
            serde_yaml::from_str("idp:\n  authorize_url: https://login.example.com/authorize\n")
                .expect("legacy settings yaml should parse");

        assert!(!legacy.idp.is_configured());
        assert!(legacy
            .idp
            .setup_hint()
            .contains("authorize_url is no longer used"));
        assert!(!IdpSection::default().setup_hint().contains("authorize_url"));
        assert!(IdpSection::default().setup_hint().contains("--oidc-issuer"));
    }

    #[test]
    fn should_drop_the_legacy_authorize_url_when_saving() {
        let legacy: RelaySettings = serde_yaml::from_str(
            "idp:\n  authorize_url: https://login.example.com/authorize\n  issuer: https://login.example.com\n  client_id: client-1\n",
        )
        .expect("settings yaml should parse");

        let saved = serde_yaml::to_string(&legacy).expect("settings should serialize");

        assert!(!saved.contains("authorize_url"), "{saved}");
        assert!(
            saved.contains("issuer: https://login.example.com"),
            "{saved}"
        );
        assert!(saved.contains("client_id: client-1"), "{saved}");
    }
}
