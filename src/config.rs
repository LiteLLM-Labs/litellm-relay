use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::apps::{default_ai_domains, default_notion_domains, domain_matches_host};
use crate::system::home_dir;

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
}

impl Default for GatewaySection {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:4000".into(),
            api_key: None,
        }
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct IdpSection {
    pub authorize_url: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct ClaudeSection {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

impl Default for ClaudeSection {
    fn default() -> Self {
        Self {
            model: "claude-sonnet-4-5".into(),
            team: None,
        }
    }
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
}
