use std::{borrow::Cow, collections::HashMap, fs, ops::Range, path::PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_saphyr::granit_parser::{Event, Marker, Parser, Span};

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
    #[serde(deserialize_with = "null_as_empty")]
    pub host: String,
    pub port: u16,
    #[serde(deserialize_with = "null_as_empty")]
    pub log_path: PathBuf,
    #[serde(deserialize_with = "null_as_empty")]
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
    #[serde(deserialize_with = "null_as_empty")]
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
    #[serde(deserialize_with = "null_as_empty")]
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
    #[serde(deserialize_with = "null_items_as_empty")]
    pub notion: Vec<String>,
    #[serde(deserialize_with = "null_items_as_empty")]
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
    #[serde(deserialize_with = "null_as_empty")]
    pub issuer: String,
    #[serde(deserialize_with = "null_as_empty")]
    pub client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_port: Option<u16>,
    #[serde(skip_serializing, deserialize_with = "null_as_empty")]
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
    #[serde(deserialize_with = "null_as_empty")]
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
    #[serde(deserialize_with = "null_as_empty")]
    pub client_id: String,
    #[serde(deserialize_with = "null_as_empty")]
    pub issuer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_port: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct CodexSection {
    #[serde(deserialize_with = "null_as_empty")]
    pub model: String,
    #[serde(deserialize_with = "null_as_empty")]
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
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "optional_null_items_as_empty"
    )]
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
        return parse_settings(&contents)
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
    let contents = settings_yaml(settings).context("failed to serialize Relay config")?;
    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    Ok(path)
}

fn parse_settings(contents: &str) -> Result<RelaySettings, serde_saphyr::Error> {
    serde_saphyr::from_str_with_options(
        &as_serde_yaml_0_9_read(contents),
        serde_saphyr::options! {
            merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
            duplicate_keys: serde_saphyr::DuplicateKeyPolicy::LastWins,
            with_snippet: false,
        },
    )
}

pub(crate) fn null_as_empty<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

fn null_items_as_empty<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(empty_for_null_items(Vec::deserialize(deserializer)?))
}

fn optional_null_items_as_empty<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::deserialize(deserializer)?.map(empty_for_null_items))
}

fn empty_for_null_items(items: Vec<Option<String>>) -> Vec<String> {
    items.into_iter().map(Option::unwrap_or_default).collect()
}

enum OpenNode {
    Mapping { next_is_key: bool },
    Sequence,
}

fn as_serde_yaml_0_9_read(contents: &str) -> Cow<'_, str> {
    let edits = serde_yaml_0_9_edits(contents);
    if edits.is_empty() {
        return Cow::Borrowed(contents);
    }
    let mut rewritten = String::with_capacity(contents.len() + 2 * edits.len());
    let mut copied = 0;
    for (range, replacement) in edits {
        rewritten.push_str(&contents[copied..range.start]);
        rewritten.push_str(&replacement);
        copied = range.end;
    }
    rewritten.push_str(&contents[copied..]);
    Cow::Owned(rewritten)
}

fn serde_yaml_0_9_edits(contents: &str) -> Vec<(Range<usize>, String)> {
    let mut parser = Parser::new_from_str(contents);
    let mut open = Vec::new();
    let mut edits = Vec::new();
    let mut previous_end = 0;
    while let Some(parsed) = parser.next_event() {
        let Ok((event, span)) = parsed else {
            return Vec::new();
        };
        let is_key = matches!(open.last(), Some(OpenNode::Mapping { next_is_key: true }));
        let since_previous = previous_end;
        previous_end = span.end.byte_offset().unwrap_or(previous_end);
        match event {
            Event::Scalar(value, ..) => {
                let key_after = is_key.then_some(since_previous);
                edits.extend(scalar_edits(contents, &value, span, key_after));
                finish_node(&mut open);
            }
            Event::Alias(_) => finish_node(&mut open),
            Event::MappingStart(..) => open.push(OpenNode::Mapping { next_is_key: true }),
            Event::SequenceStart(..) => open.push(OpenNode::Sequence),
            Event::MappingEnd | Event::SequenceEnd => {
                open.pop();
                finish_node(&mut open);
            }
            _ => {}
        }
    }
    edits
}

fn scalar_edits(
    contents: &str,
    value: &str,
    span: Span,
    key_after: Option<usize>,
) -> Vec<(Range<usize>, String)> {
    let has_value = span.start.index() < span.end.index();
    let dropped_tag = span
        .tag_start
        .filter(|_| has_value)
        .and_then(|start| tag_token(contents, start))
        .map(|range| (range, String::new()));
    let quoted_key = key_after.and_then(|key_after| match (has_value, span.tag_start) {
        (true, _) if is_null_like(value) => quoted_null_key(value, span),
        (true, _) => None,
        (false, Some(tag_start)) => quoted_tagged_empty_key(tag_start, span),
        (false, None) => quoted_empty_key(contents, key_after, span),
    });
    dropped_tag.into_iter().chain(quoted_key).collect()
}

fn quoted_tagged_empty_key(tag_start: Marker, span: Span) -> Option<(Range<usize>, String)> {
    let tag = tag_start.byte_offset()?..span.start.byte_offset()?;
    Some((tag, "''".to_string()))
}

fn quoted_null_key(value: &str, span: Span) -> Option<(Range<usize>, String)> {
    let key = span.start.byte_offset()?..span.end.byte_offset()?;
    Some((key, format!("'{value}'")))
}

fn quoted_empty_key(
    contents: &str,
    key_after: usize,
    span: Span,
) -> Option<(Range<usize>, String)> {
    let gap = contents.get(key_after..span.start.byte_offset()?)?;
    let in_comment = |at: usize| {
        gap[..at]
            .rsplit('\n')
            .next()
            .is_some_and(|line| line.contains('#'))
    };
    let (at, _) = gap.match_indices('?').find(|&(at, _)| !in_comment(at))?;
    let after_indicator = key_after + at + 1;
    Some((after_indicator..after_indicator, " ''".to_string()))
}

fn finish_node(open: &mut [OpenNode]) {
    if let Some(OpenNode::Mapping { next_is_key }) = open.last_mut() {
        *next_is_key = !*next_is_key;
    }
}

fn is_null_like(value: &str) -> bool {
    value == "~" || value.eq_ignore_ascii_case("null")
}

fn tag_token(contents: &str, start: Marker) -> Option<Range<usize>> {
    let start = start.byte_offset()?;
    let rest = contents.get(start..).filter(|rest| rest.starts_with('!'))?;
    let tag_len = rest.find(char::is_whitespace)?;
    let after_tag = &rest[tag_len..];
    let separator_len = after_tag.len() - after_tag.trim_start_matches([' ', '\t']).len();
    Some(start..start + tag_len + separator_len)
}

fn settings_yaml(settings: &RelaySettings) -> Result<String, serde_saphyr::SerializeError> {
    serde_saphyr::to_string_with_options(
        settings,
        serde_saphyr::ser_options! { prefer_block_scalars: false },
    )
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
        let settings: RelaySettings = parse_settings(
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
        let settings: RelaySettings = parse_settings(
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
            parse_settings("gateway:\n  url: https://g\n").expect("settings");
        let zero: RelaySettings =
            parse_settings("mcp:\n  max_concurrent_calls: 0\n  tool_prefix_separator: \"\"\n")
                .expect("settings");
        assert!(missing.mcp.allowed_tools().is_empty());
        assert_eq!(missing.mcp.concurrent_call_cap(), 16);
        assert_eq!(missing.mcp.tool_prefix_separator(), "-");
        assert_eq!(zero.mcp.concurrent_call_cap(), 16);
        assert_eq!(zero.mcp.tool_prefix_separator(), "-");
    }

    #[test]
    fn should_load_gateway_section_without_credential_timestamps() {
        let settings: RelaySettings = parse_settings(
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

        let yaml = settings_yaml(&settings).unwrap();
        let reloaded: RelaySettings = parse_settings(&yaml).unwrap();

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
        let yaml = settings_yaml(&RelaySettings::default()).unwrap();

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
        let settings: RelaySettings = parse_settings(
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
            parse_settings("idp:\n  authorize_url: https://login.example.com/authorize\n")
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
        let legacy: RelaySettings = parse_settings(
            "idp:\n  authorize_url: https://login.example.com/authorize\n  issuer: https://login.example.com\n  client_id: client-1\n",
        )
        .expect("settings yaml should parse");

        let saved = settings_yaml(&legacy).expect("settings should serialize");

        assert!(!saved.contains("authorize_url"), "{saved}");
        assert!(
            saved.contains("issuer: https://login.example.com"),
            "{saved}"
        );
        assert!(saved.contains("client_id: client-1"), "{saved}");
    }

    const SERDE_YAML_0_9_SAVED_CONFIG: &str = r#"relay:
  host: 127.0.0.1
  port: 4143
  log_path: /Users/dev/.litellm-relay/relay.log.jsonl
  mitm_ca_dir: /Users/dev/.litellm-relay/mitm
gateway:
  url: https://gateway.example.com
  api_key: sk-1234abcd
  enrolled_at: 2026-09-21T21:27:58Z
  expires_at: 2026-09-22T21:27:58.096Z
shadow:
  enabled: true
  model: gpt-4o-mini
  min_interval_seconds: 90
capture:
  payloads: true
  payload_preview_bytes: 8192
  payload_body_bytes: 262144
domains:
  notion: []
  ai:
  - api.openai.com
  - api.anthropic.com
timeouts:
  request_seconds: 2.5
idp:
  issuer: https://login.microsoftonline.com/tenant-id/v2.0
  client_id: client-1
  scopes: openid profile email offline_access
  redirect_port: 8765
claude:
  model: claude-sonnet-4-5
  team: team-claude
  desktop_sso:
    client_id: desktop-client
    issuer: https://login.microsoftonline.com/tenant-id/v2.0
    scopes: openid profile email
    redirect_port: 53180
codex:
  model: gpt-5-codex
  provider_id: litellm
  team: team-codex
credential:
  allowed_callers:
  - identifier: com.anthropic.claude-code
    team_id: Q6L2SF6YDW
  - identifier: codex
mcp:
  allow:
  - github-get_issue
  - Jira-searchIssues
  max_concurrent_calls: 4
  tool_prefix_separator: '-'
"#;

    #[test]
    fn should_load_every_field_of_a_config_saved_by_serde_yaml_0_9() {
        let settings =
            parse_settings(SERDE_YAML_0_9_SAVED_CONFIG).expect("settings yaml should parse");
        let issuer = "https://login.microsoftonline.com/tenant-id/v2.0";

        assert_eq!(settings.relay.host, "127.0.0.1");
        assert_eq!(settings.relay.port, 4143);
        assert_eq!(
            settings.relay.log_path,
            PathBuf::from("/Users/dev/.litellm-relay/relay.log.jsonl")
        );
        assert_eq!(
            settings.relay.mitm_ca_dir,
            PathBuf::from("/Users/dev/.litellm-relay/mitm")
        );
        assert_eq!(settings.gateway.url, "https://gateway.example.com");
        assert_eq!(settings.gateway.api_key.as_deref(), Some("sk-1234abcd"));
        assert_eq!(
            settings.gateway.enrolled_at,
            Some(
                DateTime::parse_from_rfc3339("2026-09-21T21:27:58Z")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
        assert_eq!(
            settings.gateway.expires_at,
            Some(
                DateTime::parse_from_rfc3339("2026-09-22T21:27:58.096Z")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
        assert!(settings.shadow.enabled);
        assert_eq!(settings.shadow.model, "gpt-4o-mini");
        assert_eq!(settings.shadow.min_interval_seconds, 90);
        assert!(settings.capture.payloads);
        assert_eq!(settings.capture.payload_preview_bytes, 8192);
        assert_eq!(settings.capture.payload_body_bytes, 262_144);
        assert!(settings.domains.notion.is_empty());
        assert_eq!(settings.domains.ai, ["api.openai.com", "api.anthropic.com"]);
        assert_eq!(settings.timeouts.request_seconds, 2.5);
        assert_eq!(
            settings.idp,
            IdpSection {
                issuer: issuer.into(),
                client_id: "client-1".into(),
                scopes: Some("openid profile email offline_access".into()),
                redirect_port: Some(8765),
                authorize_url: String::new(),
            }
        );
        assert_eq!(settings.claude.model, "claude-sonnet-4-5");
        assert_eq!(settings.claude.team.as_deref(), Some("team-claude"));
        assert_eq!(
            settings.claude.desktop_sso,
            Some(DesktopSso {
                client_id: "desktop-client".into(),
                issuer: issuer.into(),
                scopes: Some("openid profile email".into()),
                redirect_port: Some(53180),
            })
        );
        assert_eq!(settings.codex.model, "gpt-5-codex");
        assert_eq!(settings.codex.provider_id, "litellm");
        assert_eq!(settings.codex.team.as_deref(), Some("team-codex"));
        assert_eq!(
            settings.credential.allowed_callers,
            Some(vec![
                AllowedCaller::new("com.anthropic.claude-code", Some("Q6L2SF6YDW")),
                AllowedCaller::new("codex", None),
            ])
        );
        assert_eq!(
            settings.mcp,
            McpSection {
                allow: Some(vec!["github-get_issue".into(), "Jira-searchIssues".into()]),
                max_concurrent_calls: Some(4),
                tool_prefix_separator: Some("-".into()),
            }
        );
    }

    fn relaybar_field<'a>(saved: &'a str, section: &str, field: &str) -> Option<&'a str> {
        let mut in_section = false;
        for line in saved.lines().filter(|line| !line.is_empty()) {
            if !line.starts_with([' ', '\t']) {
                in_section = line.starts_with(&format!("{section}:"));
                continue;
            }
            if !in_section {
                continue;
            }
            if let Some(value) = line.trim().strip_prefix(&format!("{field}:")) {
                return Some(value.trim().trim_matches(['"', '\'']));
            }
        }
        None
    }

    #[test]
    fn should_reload_a_saved_config_into_the_settings_it_came_from() {
        let settings =
            parse_settings(SERDE_YAML_0_9_SAVED_CONFIG).expect("settings yaml should parse");

        let saved = settings_yaml(&settings).expect("settings should serialize");
        let reloaded = parse_settings(&saved).expect("saved settings should parse");

        assert_eq!(
            format!("{reloaded:#?}"),
            format!("{settings:#?}"),
            "{saved}"
        );
        assert!(reloaded.credential.allowed_callers.is_some(), "{saved}");
    }

    #[test]
    fn should_save_gateway_values_on_the_lines_relaybar_reads() {
        let api_key = format!(
            "eyJhbGciOiJSUzI1NiJ9.{}.{}",
            "eyJzdWIiOiJyZWxheSJ9".repeat(6),
            "c2lnbmF0dXJl".repeat(8)
        );
        let url = "https://litellm-gateway.internal.example-company.com/teams/platform/environments/production";
        let settings = RelaySettings {
            gateway: GatewaySection {
                url: url.into(),
                api_key: Some(api_key.clone()),
                ..GatewaySection::default()
            },
            ..RelaySettings::default()
        };

        let saved = settings_yaml(&settings).expect("settings should serialize");

        assert_eq!(
            relaybar_field(&saved, "gateway", "url"),
            Some(url),
            "{saved}"
        );
        assert_eq!(
            relaybar_field(&saved, "gateway", "api_key"),
            Some(api_key.as_str()),
            "{saved}"
        );
    }

    #[test]
    fn should_report_a_config_parse_error_without_echoing_the_api_key() {
        let error = parse_settings(
            "gateway:\n  url: https://g\n  api_key: sk-do-not-print\n  enrolled_at: [\n",
        )
        .expect_err("an unclosed sequence should not parse");

        let reported = format!("{error} {error:?}");
        assert!(!reported.contains("sk-do-not-print"), "{reported}");
    }

    #[test]
    fn should_ignore_a_yaml_merge_key_as_relay_always_has() {
        let settings =
            parse_settings("defaults: &defaults\n  payloads: true\ncapture:\n  <<: *defaults\n")
                .expect("settings yaml should parse");

        assert!(!settings.capture.payloads);
    }

    #[test]
    fn should_load_blank_tilde_and_null_text_fields_as_empty() {
        let settings = parse_settings(
            "relay:\n  host:\n  log_path: ~\n  mitm_ca_dir: null\n\
             gateway:\n  url:\n\
             shadow:\n  model: ~\n\
             idp:\n  issuer:\n  client_id: ~\n  authorize_url: null\n\
             claude:\n  model:\n  desktop_sso:\n    client_id: ~\n    issuer:\n\
             codex:\n  model: null\n  provider_id:\n\
             credential:\n  allowed_callers:\n  - identifier:\n",
        )
        .expect("blank, ~ and null text fields should load");

        assert_eq!(settings.relay.host, "");
        assert_eq!(settings.relay.log_path, PathBuf::new());
        assert_eq!(settings.relay.mitm_ca_dir, PathBuf::new());
        assert_eq!(settings.gateway.url, "");
        assert_eq!(settings.shadow.model, "");
        assert_eq!(settings.idp, IdpSection::default());
        assert!(!settings.idp.is_configured());
        assert_eq!(settings.claude.model, "");
        assert_eq!(
            settings.claude.desktop_sso,
            Some(DesktopSso {
                client_id: String::new(),
                issuer: String::new(),
                scopes: None,
                redirect_port: None,
            })
        );
        assert_eq!(settings.codex.model, "");
        assert_eq!(settings.codex.provider_id, "");
        assert_eq!(
            settings.credential.allowed_callers,
            Some(vec![AllowedCaller::new("", None)])
        );
    }

    #[test]
    fn should_load_blank_tilde_and_null_list_items_as_empty() {
        let settings = parse_settings(
            "domains:\n  ai:\n  -\n  - ~\n  notion:\n  - null\n  - notion.so\n\
             mcp:\n  allow:\n  - ~\n  - github-get_issue\n",
        )
        .expect("blank, ~ and null list items should load");

        assert_eq!(settings.domains.ai, vec![String::new(), String::new()]);
        assert_eq!(
            settings.domains.notion,
            vec![String::new(), "notion.so".to_string()]
        );
        assert_eq!(
            settings.mcp.allow,
            Some(vec![String::new(), "github-get_issue".to_string()])
        );
    }

    #[test]
    fn should_load_core_tagged_values_by_field_type_as_serde_yaml_0_9_did() {
        let settings = parse_settings(
            "relay:\n  port: !!str 4142\n  host: !!int 127\n\
             gateway:\n  url: !!float 1.5\n  enrolled_at: !!timestamp 2026-09-21T21:27:58Z\n\
             shadow:\n  enabled: !!str true\n  model: !str gpt\n\
             capture:\n  payloads: !!bool yes\n  payload_body_bytes: !!int 10\n\
             timeouts:\n  request_seconds: !degrees 180\n\
             idp:\n  issuer: !!bool true\n  client_id: !<tag:yaml.org,2002:int> 5\n  redirect_port: !!float 8765\n\
             mcp:\n  allow: [!!int 7, !!str x, !!null]\n  max_concurrent_calls: !!str 4\n",
        )
        .expect("core-tagged values should load");

        assert_eq!(settings.relay.port, 4142);
        assert_eq!(settings.relay.host, "127");
        assert_eq!(settings.gateway.url, "1.5");
        assert_eq!(
            settings.gateway.enrolled_at,
            Some("2026-09-21T21:27:58Z".parse().expect("timestamp"))
        );
        assert!(settings.shadow.enabled);
        assert_eq!(settings.shadow.model, "gpt");
        assert!(settings.capture.payloads);
        assert_eq!(settings.capture.payload_body_bytes, 10);
        assert_eq!(settings.timeouts.request_seconds, 180.0);
        assert_eq!(settings.idp.issuer, "true");
        assert_eq!(settings.idp.client_id, "5");
        assert_eq!(settings.idp.redirect_port, Some(8765));
        assert_eq!(
            settings.mcp.allow,
            Some(vec!["7".into(), "x".into(), String::new()])
        );
        assert_eq!(settings.mcp.max_concurrent_calls, Some(4));
    }

    #[test]
    fn should_ignore_a_null_mapping_key_as_serde_yaml_0_9_did() {
        let settings = parse_settings(
            "~: x\nnull: y\n?\n: w\n? !!str\n: u\n? !!null # c\n: t\n!x : s\n\
             gateway:\n  ~: z\n  url: https://g # why?\n  ?\n  : v\nshadow: {!!null, model: m}\n",
        )
        .expect("a null key should be ignored like any unknown key");

        assert_eq!(settings.gateway.url, "https://g");
        assert_eq!(settings.shadow.model, "m");
    }

    #[test]
    fn should_load_tagged_mapping_keys_as_serde_yaml_0_9_did() {
        let settings = parse_settings(
            "!!str\trelay:\n  !!str port: 4142\n  host: h\n\
             !!int null: 5\n\
             credential:\n  allowed_callers:\n  - !!str identifier: com.example.app\n    team_id: T1\n",
        )
        .expect("tagged keys with siblings should load");

        assert_eq!(settings.relay.port, 4142);
        assert_eq!(settings.relay.host, "h");
        assert_eq!(
            settings.credential.allowed_callers,
            Some(vec![AllowedCaller::new("com.example.app", Some("T1"))])
        );
    }

    #[test]
    fn should_keep_the_last_value_of_a_repeated_key() {
        let settings = parse_settings(
            "notes: a\nnotes: b\n~: c\n~: d\n\
             defaults: &defaults\n  payloads: true\n\
             capture:\n  <<: *defaults\n  <<: *defaults\n\
             gateway:\n  url: https://first\n  url: https://last\n\
             credential:\n  allowed_callers:\n  - identifier: com.example.app\n    notes: x\n    notes: y\n",
        )
        .expect("repeated keys should load");

        assert_eq!(settings.gateway.url, "https://last");
        assert!(!settings.capture.payloads);
        assert_eq!(
            settings.credential.allowed_callers,
            Some(vec![AllowedCaller::new("com.example.app", None)])
        );
    }

    #[test]
    fn should_keep_a_binary_tagged_text_field_as_written() {
        let settings = parse_settings("gateway:\n  url: !!binary aHR0cHM6Ly9leGFtcGxlLmNvbQ==\n")
            .expect("settings yaml should parse");

        assert_eq!(settings.gateway.url, "aHR0cHM6Ly9leGFtcGxlLmNvbQ==");
    }
}
