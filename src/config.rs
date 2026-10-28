use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_saphyr::granit_parser::{Event, Marker, Parser, ScalarStyle, Span, Tag};

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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub environments: Vec<EnvironmentEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct EnvironmentEntry {
    pub name: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
}

fn without_trailing_slash(url: &str) -> &str {
    url.strip_suffix('/').unwrap_or(url)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub enum CarriedSelection {
    Carried {
        settings: Box<RelaySettings>,
        summary: String,
    },
    Nothing(String),
}

impl RelaySettings {
    pub fn current_environment(&self) -> Option<&EnvironmentEntry> {
        self.environment_with_url(&self.gateway.url)
    }

    pub fn environment_with_url(&self, url: &str) -> Option<&EnvironmentEntry> {
        let wanted = without_trailing_slash(url.trim());
        self.environments
            .iter()
            .find(|entry| without_trailing_slash(entry.url.trim()) == wanted)
    }

    pub fn environment_named(&self, name: &str) -> Option<&EnvironmentEntry> {
        self.environments.iter().find(|entry| entry.name == name)
    }

    pub fn managed_team(&self) -> Option<String> {
        let selected = non_empty(self.gateway.team.as_deref());
        let environment = non_empty(
            self.current_environment()
                .and_then(|entry| entry.team.as_deref()),
        );
        let on_the_default_gateway = self.current_environment().is_none_or(|current| {
            self.environments
                .first()
                .is_some_and(|first| first.name == current.name)
        });
        let claude = non_empty(self.claude.team.as_deref()).filter(|_| on_the_default_gateway);
        let codex = non_empty(self.codex.team.as_deref()).filter(|_| on_the_default_gateway);
        if let (None, None, Some(claude), Some(codex)) = (selected, environment, claude, codex) {
            if claude != codex {
                eprintln!(
                    "broker: claude.team ({claude}) and codex.team ({codex}) differ; minting keys for {claude}"
                );
            }
        }
        selected
            .or(environment)
            .or(claude)
            .or(codex)
            .map(str::to_string)
    }

    pub fn carrying_selection_from(self, previous: &RelaySettings) -> CarriedSelection {
        let Some(picked) = previous.current_environment() else {
            return self.carrying_team_on_the_same_gateway(previous);
        };
        let Some(kept) = self.environment_named(&picked.name).cloned() else {
            return CarriedSelection::Nothing(format!(
                "the new config does not name the environment {}",
                picked.name
            ));
        };
        let parked: BTreeMap<String, String> = previous
            .gateway
            .team_by_environment
            .iter()
            .filter(|(name, _)| self.environment_named(name).is_some())
            .map(|(name, team)| (name.clone(), team.clone()))
            .collect();
        let picked_team = previous.gateway.team.clone();
        let on_the_default_environment = self.current_environment() == Some(&kept);
        let unchanged = on_the_default_environment && picked_team.is_none() && parked.is_empty();
        if unchanged {
            return CarriedSelection::Nothing(format!(
                "the environment {} is the new config's default and no team was picked",
                kept.name
            ));
        }
        let parked_names = parked.keys().cloned().collect::<Vec<_>>().join(", ");
        let summary = format!(
            "carried the environment {}{}{}",
            kept.name,
            picked_team
                .as_deref()
                .map(|team| format!(" with the team {team}"))
                .unwrap_or_default(),
            match parked_names.is_empty() {
                true => String::new(),
                false => format!(" and the team picks for {parked_names}"),
            }
        );
        let team = picked_team.or_else(|| {
            self.gateway
                .team
                .clone()
                .filter(|_| on_the_default_environment)
        });
        let gateway = GatewaySection {
            url: kept.url,
            team,
            team_by_environment: parked,
            ..self.gateway
        };
        CarriedSelection::Carried {
            settings: Box::new(RelaySettings { gateway, ..self }),
            summary,
        }
    }

    pub fn carrying_picks_over(
        self,
        previous: &RelaySettings,
        seeded: &RelaySettings,
    ) -> CarriedSelection {
        let picks = previous.without_seeded_picks(seeded);
        let nothing_picked = picks.gateway.team.is_none()
            && picks.gateway.team_by_environment.is_empty()
            && picks.on_the_gateway_of(seeded);
        if nothing_picked {
            return CarriedSelection::Nothing(
                "no environment or team was picked over the earlier managed config".to_string(),
            );
        }
        let default_environment = self.current_environment().map(|entry| entry.name.clone());
        let managed_team = non_empty(self.gateway.team.as_deref()).map(str::to_string);
        match self.carrying_selection_from(&picks) {
            CarriedSelection::Carried { settings, summary } => CarriedSelection::Carried {
                settings: Box::new(settings.parking_team(default_environment, managed_team)),
                summary,
            },
            nothing => nothing,
        }
    }

    pub fn carrying_install_picks(
        self,
        previous: &RelaySettings,
        seeded: Option<&RelaySettings>,
    ) -> CarriedSelection {
        match seeded {
            Some(seeded) => self.carrying_picks_over(previous, seeded),
            None => self.carrying_selection_from(&RelaySettings {
                gateway: GatewaySection {
                    team: None,
                    ..previous.gateway.clone()
                },
                ..previous.clone()
            }),
        }
    }

    fn parking_team(self, environment: Option<String>, team: Option<String>) -> RelaySettings {
        let Some((environment, team)) = environment.zip(team) else {
            return self;
        };
        let away = self
            .current_environment()
            .is_some_and(|current| current.name != environment);
        if !away || self.gateway.team_by_environment.contains_key(&environment) {
            return self;
        }
        let team_by_environment = self
            .gateway
            .team_by_environment
            .iter()
            .map(|(name, team)| (name.clone(), team.clone()))
            .chain([(environment, team)])
            .collect();
        RelaySettings {
            gateway: GatewaySection {
                team_by_environment,
                ..self.gateway
            },
            ..self
        }
    }

    fn on_the_gateway_of(&self, other: &RelaySettings) -> bool {
        match (self.current_environment(), other.current_environment()) {
            (Some(mine), Some(theirs)) => mine.name == theirs.name,
            (None, None) => {
                without_trailing_slash(self.gateway.url.trim())
                    == without_trailing_slash(other.gateway.url.trim())
            }
            _ => false,
        }
    }

    fn without_seeded_picks(&self, seeded: &RelaySettings) -> RelaySettings {
        let seeded_environment = seeded
            .current_environment()
            .map(|entry| entry.name.as_str());
        let seeded_team = non_empty(seeded.gateway.team.as_deref());
        let seeded_pick = |environment: Option<&str>, team: &str| {
            seeded_team.is_some()
                && seeded_team == non_empty(Some(team))
                && environment == seeded_environment
        };
        let on_the_seeded_gateway = self.on_the_gateway_of(seeded);
        let current = self.current_environment().map(|entry| entry.name.as_str());
        let team = self
            .gateway
            .team
            .clone()
            .filter(|team| !(on_the_seeded_gateway && seeded_pick(current, team)));
        let team_by_environment = self
            .gateway
            .team_by_environment
            .iter()
            .filter(|(name, team)| !seeded_pick(Some(name), team))
            .map(|(name, team)| (name.clone(), team.clone()))
            .collect();
        RelaySettings {
            gateway: GatewaySection {
                team,
                team_by_environment,
                ..self.gateway.clone()
            },
            ..self.clone()
        }
    }

    fn carrying_team_on_the_same_gateway(self, previous: &RelaySettings) -> CarriedSelection {
        let same_gateway = without_trailing_slash(self.gateway.url.trim())
            == without_trailing_slash(previous.gateway.url.trim());
        if !same_gateway {
            return CarriedSelection::Nothing(format!(
                "the new config does not name the previous Gateway {}",
                previous.gateway.url
            ));
        }
        let Some(team) = previous.gateway.team.clone() else {
            return CarriedSelection::Nothing("no team was picked".to_string());
        };
        let summary = format!("carried the team {team} on {}", self.gateway.url);
        let gateway = GatewaySection {
            team: Some(team),
            ..self.gateway
        };
        CarriedSelection::Carried {
            settings: Box::new(RelaySettings { gateway, ..self }),
            summary,
        }
    }

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub team_by_environment: BTreeMap<String, String>,
}

impl Default for GatewaySection {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:4000".into(),
            api_key: None,
            enrolled_at: None,
            expires_at: None,
            team: None,
            team_by_environment: BTreeMap::new(),
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

fn read_settings_file(path: &Path) -> Result<RelaySettings> {
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    parse_settings(&contents).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn carry_selection(previous: &Path, seeded: Option<&Path>) -> Result<String> {
    let previous = read_settings_file(previous)?;
    let current = load_settings()?;
    let seeded = seeded.map(read_settings_file).transpose()?;
    match current.carrying_install_picks(&previous, seeded.as_ref()) {
        CarriedSelection::Carried { settings, summary } => {
            save_settings(&settings)?;
            Ok(summary)
        }
        CarriedSelection::Nothing(reason) => Ok(format!("carried nothing: {reason}")),
    }
}

pub fn save_settings(settings: &RelaySettings) -> Result<PathBuf> {
    let path = config_path();
    save_settings_to(&path, settings)?;
    Ok(path)
}

fn save_settings_to(path: &Path, settings: &RelaySettings) -> Result<()> {
    let contents = settings_yaml(settings).context("failed to serialize Relay config")?;
    if is_saved_owner_only(path, &contents) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let staged = staged_path(path);
    let written = write_owner_only(&staged, &contents)
        .and_then(|()| keep_owner(path, &staged))
        .and_then(|()| {
            fs::rename(&staged, path)
                .with_context(|| format!("failed to replace {}", path.display()))
        });
    if written.is_err() {
        let _ = fs::remove_file(&staged);
    }
    written
}

fn staged_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "config.yaml".into(), |name| name.to_string_lossy());
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

#[cfg(unix)]
fn keep_owner(path: &Path, staged: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let owner = fs::symlink_metadata(path)
        .or_else(|_| fs::metadata(path.parent().unwrap_or(Path::new("."))))
        .with_context(|| format!("failed to read the owner of {}", path.display()))?;
    let made =
        fs::metadata(staged).with_context(|| format!("failed to read {}", staged.display()))?;
    if (made.uid(), made.gid()) == (owner.uid(), owner.gid()) {
        return Ok(());
    }
    std::os::unix::fs::chown(staged, Some(owner.uid()), Some(owner.gid()))
        .with_context(|| format!("failed to give {} its owner back", path.display()))
}

#[cfg(not(unix))]
fn keep_owner(_path: &Path, _staged: &Path) -> Result<()> {
    Ok(())
}

fn write_owner_only(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    let _ = fs::remove_file(path);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    file.write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .with_context(|| format!("failed to write {}", path.display()))
}

fn is_saved_owner_only(path: &Path, contents: &str) -> bool {
    if !fs::read_to_string(path).is_ok_and(|saved| saved == contents) {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|metadata| metadata.permissions().mode() & 0o777 == 0o600)
    }
    #[cfg(not(unix))]
    {
        true
    }
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
            Event::Scalar(value, style, anchor_id, tag) => {
                let scalar = ScalarAt {
                    value: &value,
                    is_null_word: style == ScalarStyle::Plain && is_null_like(&value),
                    anchored: anchor_id != 0,
                    tag: tag.as_deref(),
                    span,
                };
                let key_after = is_key.then_some(since_previous);
                edits.extend(scalar_edits(contents, scalar, key_after));
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

#[derive(Clone, Copy)]
struct ScalarAt<'a> {
    value: &'a str,
    is_null_word: bool,
    anchored: bool,
    tag: Option<&'a Tag>,
    span: Span,
}

fn scalar_edits(
    contents: &str,
    scalar: ScalarAt,
    key_after: Option<usize>,
) -> Vec<(Range<usize>, String)> {
    let ScalarAt {
        value,
        is_null_word,
        anchored,
        tag,
        span,
    } = scalar;
    let has_value = span.start.index() < span.end.index();
    let tagged_null = tag.is_some_and(|tag| tag.core_suffix() == Some("null"));
    match (span.tag_start, has_value) {
        (None, true) => key_after
            .filter(|_| is_null_word)
            .and_then(|_| quoted_null_word(value, span))
            .into_iter()
            .collect(),
        (None, false) => key_after
            .and_then(|key_after| quoted_empty_key(contents, key_after, span, anchored))
            .into_iter()
            .collect(),
        (Some(tag_start), true) => {
            let reads_as_text = is_null_word && (key_after.is_some() || !tagged_null);
            tag_token(contents, tag_start)
                .map(|range| (range, String::new()))
                .into_iter()
                .chain(
                    reads_as_text
                        .then(|| quoted_null_word(value, span))
                        .flatten(),
                )
                .collect()
        }
        (Some(tag_start), false) => {
            let empty = if key_after.is_some() { "''" } else { "~" };
            untagged_empty_node(contents, tag_start, span, empty)
                .into_iter()
                .collect()
        }
    }
}

fn untagged_empty_node(
    contents: &str,
    tag_start: Marker,
    span: Span,
    empty: &str,
) -> Option<(Range<usize>, String)> {
    let properties = tag_start.byte_offset()?..span.start.byte_offset()?;
    let other_properties = contents.get(tag_end(contents, properties.start)?..properties.end)?;
    let node = match other_properties {
        "" => empty.to_string(),
        anchor => format!("{anchor} {empty}"),
    };
    Some((properties, node))
}

fn quoted_null_word(value: &str, span: Span) -> Option<(Range<usize>, String)> {
    let word = span.start.byte_offset()?..span.end.byte_offset()?;
    Some((word, format!("'{value}'")))
}

fn quoted_empty_key(
    contents: &str,
    key_after: usize,
    span: Span,
    anchored: bool,
) -> Option<(Range<usize>, String)> {
    let key_start = span.start.byte_offset()?;
    let gap = contents.get(key_after..key_start)?;
    let in_comment = |at: usize| {
        gap[..at]
            .rsplit('\n')
            .next()
            .is_some_and(|line| line.contains('#'))
    };
    let (at, _) = gap.match_indices('?').find(|&(at, _)| !in_comment(at))?;
    let key_end = if anchored {
        key_start
    } else {
        key_after + at + 1
    };
    Some((key_end..key_end, " ''".to_string()))
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
    let end = tag_end(contents, start)?;
    let separator_len =
        contents[end..].len() - contents[end..].trim_start_matches([' ', '\t']).len();
    Some(start..end + separator_len)
}

fn tag_end(contents: &str, start: usize) -> Option<usize> {
    let tag = contents.get(start..).filter(|rest| rest.starts_with('!'))?;
    let tag_len = match tag.strip_prefix("!<") {
        Some(verbatim) => verbatim.find('>')? + 3,
        None => tag
            .find(|c: char| c.is_whitespace() || ",[]{}".contains(c))
            .unwrap_or(tag.len()),
    };
    Some(start + tag_len)
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
                ..GatewaySection::default()
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

    const THREE_ENVIRONMENTS: &str = r#"
gateway:
  url: https://uat.example.com/
  team: picked
environments:
  - name: prod
    url: https://prod.example.com
    team: prod-team
  - name: uat
    url: https://uat.example.com
  - name: test
    url: https://test.example.com/
    team: test-team
claude:
  team: claude-team
codex:
  team: codex-team
"#;

    #[test]
    fn should_parse_the_environments_list_and_the_selected_team() {
        let settings: RelaySettings = parse_settings(THREE_ENVIRONMENTS).expect("settings");

        assert_eq!(settings.gateway.team.as_deref(), Some("picked"));
        assert_eq!(
            settings
                .environments
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["prod", "uat", "test"]
        );
        assert_eq!(
            settings.environments[2],
            EnvironmentEntry {
                name: "test".into(),
                url: "https://test.example.com/".into(),
                team: Some("test-team".into()),
            }
        );
        assert_eq!(settings.environments[1].team, None);
        assert_eq!(
            settings.environment_named("test"),
            Some(&settings.environments[2])
        );
        assert_eq!(
            settings
                .environment_named("prod")
                .map(|entry| entry.url.as_str()),
            Some("https://prod.example.com")
        );
        assert_eq!(settings.environment_named("staging"), None);
        assert_eq!(settings.environment_named("Prod"), None);
    }

    #[test]
    fn should_match_the_current_environment_by_url_ignoring_one_trailing_slash() {
        let mut settings: RelaySettings = parse_settings(THREE_ENVIRONMENTS).expect("settings");
        assert_eq!(settings.current_environment().unwrap().name, "uat");

        settings.gateway.url = "https://uat.example.com".into();
        assert_eq!(settings.current_environment().unwrap().name, "uat");

        settings.gateway.url = "https://test.example.com".into();
        assert_eq!(settings.current_environment().unwrap().name, "test");

        settings.gateway.url = "https://elsewhere.example.com".into();
        assert_eq!(settings.current_environment(), None);

        settings.gateway.url = "https://uat.example.com//".into();
        assert_eq!(settings.current_environment(), None);

        assert_eq!(RelaySettings::default().current_environment(), None);
    }

    #[test]
    fn should_pick_the_managed_team_from_gateway_then_environment_then_tool_teams_on_the_default_gateway(
    ) {
        let mut settings: RelaySettings = parse_settings(THREE_ENVIRONMENTS).expect("settings");
        settings.gateway.url = "https://prod.example.com".into();
        assert_eq!(settings.managed_team().as_deref(), Some("picked"));

        settings.gateway.team = None;
        assert_eq!(settings.managed_team().as_deref(), Some("prod-team"));

        settings.environments[0].team = None;
        assert_eq!(settings.managed_team().as_deref(), Some("claude-team"));

        settings.gateway.url = "https://uat.example.com".into();
        assert_eq!(settings.managed_team(), None);

        settings.gateway.url = "https://elsewhere.example.com".into();
        assert_eq!(settings.managed_team().as_deref(), Some("claude-team"));

        settings.claude.team = Some("  ".into());
        assert_eq!(settings.managed_team().as_deref(), Some("codex-team"));

        settings.codex.team = None;
        assert_eq!(settings.managed_team(), None);

        settings.gateway.team = Some(" picked ".into());
        assert_eq!(settings.managed_team().as_deref(), Some("picked"));
    }

    #[test]
    fn should_save_a_config_without_environments_or_a_picked_team_with_none_of_their_keys() {
        let settings: RelaySettings =
            parse_settings(include_str!("../mdm/config.yaml.example")).expect("settings");

        let saved = settings_yaml(&settings).expect("settings should serialize");

        let keys: Vec<&str> = saved
            .lines()
            .filter_map(|line| line.trim_start().split(':').next())
            .collect();
        for absent in ["environments", "team", "team_by_environment"] {
            assert!(!keys.contains(&absent), "{absent} saved in {saved}");
        }
        let reloaded = parse_settings(&saved).expect("saved settings parse");
        assert_eq!(
            format!("{reloaded:#?}"),
            format!("{settings:#?}"),
            "{saved}"
        );
    }

    #[test]
    fn should_round_trip_environments_and_the_picked_team_through_yaml() {
        let settings: RelaySettings = parse_settings(THREE_ENVIRONMENTS).expect("settings");

        let saved = settings_yaml(&settings).expect("settings should serialize");
        let reloaded: RelaySettings = parse_settings(&saved).expect("saved settings parse");

        assert_eq!(reloaded.environments, settings.environments);
        assert_eq!(reloaded.gateway.team.as_deref(), Some("picked"));
        assert_eq!(reloaded.gateway.url, "https://uat.example.com/");
    }

    fn carried(outcome: CarriedSelection) -> (RelaySettings, String) {
        match outcome {
            CarriedSelection::Carried { settings, summary } => (*settings, summary),
            CarriedSelection::Nothing(reason) => panic!("expected a carry, got: {reason}"),
        }
    }

    fn carried_nothing(outcome: CarriedSelection) -> String {
        match outcome {
            CarriedSelection::Carried { summary, .. } => panic!("expected nothing, got: {summary}"),
            CarriedSelection::Nothing(reason) => reason,
        }
    }

    #[test]
    fn should_never_show_a_reader_a_half_written_config() {
        let dir = std::env::temp_dir().join(format!("relay-config-race-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.yaml");
        let first: RelaySettings = parse_settings(THREE_ENVIRONMENTS).expect("settings");
        let mut second = first.clone();
        second.gateway.team = Some("another".to_string());
        save_settings_to(&path, &first).expect("first save");
        let expected =
            [first.clone(), second.clone()].map(|settings| settings_yaml(&settings).unwrap());

        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = {
            let (path, stop) = (path.clone(), std::sync::Arc::clone(&stop));
            std::thread::spawn(move || {
                let reads = std::iter::from_fn(|| {
                    (!stop.load(std::sync::atomic::Ordering::SeqCst))
                        .then(|| fs::read_to_string(&path).unwrap_or_default())
                });
                reads.filter(|read| !expected.contains(read)).count()
            })
        };
        for round in 0..2000 {
            let settings = if round % 2 == 0 { &second } else { &first };
            save_settings_to(&path, settings).expect("save");
        }
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(reader.join().expect("reader"), 0);
        let staged: Vec<_> = fs::read_dir(&dir)
            .expect("dir")
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .collect();
        assert_eq!(staged, vec![std::ffi::OsString::from("config.yaml")]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn should_leave_config_yaml_untouched_when_a_save_changes_nothing() {
        let dir = std::env::temp_dir().join(format!("relay-config-save-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.yaml");
        let settings: RelaySettings = parse_settings(THREE_ENVIRONMENTS).expect("settings");
        save_settings_to(&path, &settings).expect("first save");
        let long_ago = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|file| file.set_modified(long_ago))
            .expect("backdate");

        save_settings_to(&path, &settings).expect("unchanged save");
        let after_unchanged = fs::metadata(&path)
            .and_then(|m| m.modified())
            .expect("mtime");

        let mut moved = settings.clone();
        moved.gateway.team = Some("another-team".into());
        save_settings_to(&path, &moved).expect("changed save");
        let reread: RelaySettings =
            parse_settings(&fs::read_to_string(&path).expect("read")).expect("parse");
        fs::remove_dir_all(&dir).expect("cleanup");

        assert_eq!(after_unchanged, long_ago);
        assert_eq!(reread.gateway.team.as_deref(), Some("another-team"));
    }

    #[test]
    fn should_carry_the_picked_environment_and_teams_into_a_new_managed_config_by_name() {
        let mut previous: RelaySettings = parse_settings(THREE_ENVIRONMENTS).expect("settings");
        previous.gateway.team_by_environment = BTreeMap::from([
            ("prod".into(), "prod-pick".into()),
            ("gone".into(), "x".into()),
        ]);
        let managed: RelaySettings = parse_settings(
            "gateway:\n  url: https://prod.example.com\nenvironments:\n  - name: prod\n    url: https://prod.example.com\n  - name: uat\n    url: https://uat-v2.example.com\nclaude:\n  model: new-model\n",
        )
        .expect("settings");

        let (settings, summary) = carried(managed.carrying_selection_from(&previous));

        assert_eq!(settings.gateway.url, "https://uat-v2.example.com");
        assert_eq!(settings.current_environment().unwrap().name, "uat");
        assert_eq!(settings.gateway.team.as_deref(), Some("picked"));
        assert_eq!(
            settings.gateway.team_by_environment,
            BTreeMap::from([("prod".into(), "prod-pick".into())])
        );
        assert_eq!(settings.claude.model, "new-model");
        assert_eq!(
            summary,
            "carried the environment uat with the team picked and the team picks for prod"
        );
    }

    #[test]
    fn should_let_a_re_pushed_managed_team_win_over_the_team_the_earlier_push_seeded() {
        let seeded_yaml = "gateway:\n  url: https://prod.example.com\n  team: managed-1\nenvironments:\n  - name: prod\n    url: https://prod.example.com\n  - name: uat\n    url: https://uat.example.com\n";
        let seeded: RelaySettings = parse_settings(seeded_yaml).expect("settings");
        let repushed = || -> RelaySettings {
            parse_settings(&seeded_yaml.replace("managed-1", "managed-2")).expect("settings")
        };

        assert_eq!(
            carried_nothing(repushed().carrying_picks_over(&seeded, &seeded)),
            "no environment or team was picked over the earlier managed config"
        );

        let mut picked_team = seeded.clone();
        picked_team.gateway.team = Some("mine".into());
        let (settings, _) = carried(repushed().carrying_picks_over(&picked_team, &seeded));
        assert_eq!(settings.gateway.team.as_deref(), Some("mine"));

        let mut moved = seeded.clone();
        moved.gateway.url = "https://uat.example.com".into();
        moved.gateway.team = None;
        moved.gateway.team_by_environment = BTreeMap::from([("prod".into(), "managed-1".into())]);
        let (settings, summary) = carried(repushed().carrying_picks_over(&moved, &seeded));
        assert_eq!(settings.current_environment().unwrap().name, "uat");
        assert_eq!(settings.gateway.team, None);
        assert_eq!(
            settings.gateway.team_by_environment,
            BTreeMap::from([("prod".into(), "managed-2".into())])
        );
        assert_eq!(summary, "carried the environment uat");

        let mut parked_pick = moved.clone();
        parked_pick.gateway.team_by_environment = BTreeMap::from([("prod".into(), "mine".into())]);
        let (settings, _) = carried(repushed().carrying_picks_over(&parked_pick, &seeded));
        assert_eq!(
            settings.gateway.team_by_environment,
            BTreeMap::from([("prod".into(), "mine".into())])
        );

        let (settings, _) = carried(repushed().carrying_selection_from(&moved));
        assert_eq!(
            settings.gateway.team_by_environment,
            BTreeMap::from([("prod".into(), "managed-1".into())])
        );

        let mut came_back = seeded.clone();
        came_back.gateway.team_by_environment = BTreeMap::from([("uat".into(), "uat-pick".into())]);
        let (settings, summary) = carried(repushed().carrying_picks_over(&came_back, &seeded));
        assert_eq!(settings.current_environment().unwrap().name, "prod");
        assert_eq!(settings.gateway.team.as_deref(), Some("managed-2"));
        assert_eq!(
            settings.gateway.team_by_environment,
            BTreeMap::from([("uat".into(), "uat-pick".into())])
        );
        assert_eq!(
            summary,
            "carried the environment prod and the team picks for uat"
        );

        let mut never_seeded = came_back.clone();
        never_seeded.gateway.team = None;
        let (settings, _) = carried(repushed().carrying_selection_from(&never_seeded));
        assert_eq!(settings.gateway.team.as_deref(), Some("managed-2"));
    }

    #[test]
    fn should_let_the_first_managed_team_win_when_an_earlier_install_left_no_seeded_copy() {
        let earlier_yaml = "gateway:\n  url: https://prod.example.com\n  team: managed-1\nenvironments:\n  - name: prod\n    url: https://prod.example.com\n  - name: uat\n    url: https://uat.example.com\n";
        let earlier: RelaySettings = parse_settings(earlier_yaml).expect("settings");
        let pushed = || -> RelaySettings {
            parse_settings(&earlier_yaml.replace("managed-1", "managed-2")).expect("settings")
        };

        assert_eq!(
            carried_nothing(pushed().carrying_install_picks(&earlier, None)),
            "the environment prod is the new config's default and no team was picked"
        );
        assert_eq!(
            carried_nothing(pushed().carrying_install_picks(&pushed(), Some(&pushed()))),
            "no environment or team was picked over the earlier managed config"
        );

        let mut on_uat = earlier.clone();
        on_uat.gateway.url = "https://uat.example.com".into();
        on_uat.gateway.team_by_environment = BTreeMap::from([("prod".into(), "mine".into())]);
        let (settings, summary) = carried(pushed().carrying_install_picks(&on_uat, None));
        assert_eq!(settings.current_environment().unwrap().name, "uat");
        assert_eq!(settings.gateway.team, None);
        assert_eq!(
            settings.gateway.team_by_environment,
            BTreeMap::from([("prod".into(), "mine".into())])
        );
        assert_eq!(
            summary,
            "carried the environment uat and the team picks for prod"
        );
    }

    #[test]
    fn should_carry_nothing_when_the_new_config_drops_the_picked_environment() {
        let previous: RelaySettings = parse_settings(THREE_ENVIRONMENTS).expect("settings");
        let managed: RelaySettings = parse_settings(
            "gateway:\n  url: https://prod.example.com\nenvironments:\n  - name: prod\n    url: https://prod.example.com\n",
        )
        .expect("settings");

        assert_eq!(
            carried_nothing(managed.carrying_selection_from(&previous)),
            "the new config does not name the environment uat"
        );
    }

    #[test]
    fn should_carry_the_team_of_a_single_gateway_config_only_while_the_url_stays() {
        let previous: RelaySettings =
            parse_settings("gateway:\n  url: https://gw.example.com/\n  team: eng\n")
                .expect("settings");
        let same: RelaySettings =
            parse_settings("gateway:\n  url: https://gw.example.com\n").expect("settings");
        let moved: RelaySettings =
            parse_settings("gateway:\n  url: https://gw2.example.com\n").expect("settings");
        let with_environments: RelaySettings = parse_settings(
            "gateway:\n  url: https://gw.example.com\nenvironments:\n  - name: prod\n    url: https://gw.example.com\n    team: prod-default\n  - name: uat\n    url: https://uat.example.com\n",
        )
        .expect("settings");

        let (settings, summary) = carried(same.carrying_selection_from(&previous));
        assert_eq!(settings.gateway.team.as_deref(), Some("eng"));
        assert_eq!(settings.gateway.url, "https://gw.example.com");
        assert_eq!(summary, "carried the team eng on https://gw.example.com");

        let (settings, _) = carried(with_environments.carrying_selection_from(&previous));
        assert_eq!(settings.current_environment().unwrap().name, "prod");
        assert_eq!(settings.managed_team().as_deref(), Some("eng"));

        assert_eq!(
            carried_nothing(moved.carrying_selection_from(&previous)),
            "the new config does not name the previous Gateway https://gw.example.com/"
        );
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
            "~: x\nnull: y\n?\n: w\n? !!str\n: u\n? !!null # c\n: t\n!x : s\n!!null ~: r\n\
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

    #[test]
    fn should_load_a_tagged_null_word_as_text_as_serde_yaml_0_9_did() {
        let settings = parse_settings(
            "gateway:\n  url: https://g\n  api_key: !!str null\n\
             idp:\n  issuer: !!str ~\n  client_id: !foo NULL\n\
             claude:\n  team: !!null null\n\
             codex:\n  team: !!str &t Null\n\
             domains:\n  ai: [!!str null, !<tag:yaml.org,2002:str> ~, *t]\n  notion:\n  - !!str |-\n    ~\n",
        )
        .expect("tagged null words should load");

        assert_eq!(settings.gateway.api_key.as_deref(), Some("null"));
        assert_eq!(settings.idp.issuer, "~");
        assert_eq!(settings.idp.client_id, "NULL");
        assert_eq!(settings.claude.team, None);
        assert_eq!(settings.codex.team.as_deref(), Some("Null"));
        assert_eq!(settings.domains.ai, vec!["null", "~", "Null"]);
        assert_eq!(settings.domains.notion, vec!["~"]);
    }

    #[test]
    fn should_read_a_tagged_empty_value_like_an_untagged_one() {
        let tagged = parse_settings(
            "relay: !!map\n\
             gateway:\n  url: https://g\n  api_key: !!str\n\
             credential: !!map &c # note\n\
             claude:\n  team: *c\n\
             domains:\n  ai: !!seq\n  notion: [!!str &n, a, *n]\n\
             idp: !<tag:yaml.org,2002:map>\n\
             codex: !foo\n\
             mcp:\n  allow: [!!str, b]\n",
        )
        .expect("tagged empty values should load");
        let untagged = parse_settings(
            "relay:\n\
             gateway:\n  url: https://g\n  api_key:\n\
             credential: &c # note\n\
             claude:\n  team: *c\n\
             domains:\n  ai:\n  notion: [&n ~, a, *n]\n\
             idp:\n\
             codex:\n\
             mcp:\n  allow: [~, b]\n",
        )
        .expect("untagged empty values should load");

        assert_eq!(format!("{tagged:?}"), format!("{untagged:?}"));
        assert_eq!(tagged.domains.notion, vec!["", "a", ""]);
    }

    #[test]
    fn should_ignore_an_anchored_empty_key_like_any_unknown_key() {
        let settings = parse_settings(
            "? &a # c\n: x\n? &b\t\n: y\n?\t&e\n: v\n?\n  &f\n: w\n\
             relay:\n  ? &c\n  : z\n  port: 4142\n\
             shadow: {? &d, model: m}\n\
             gateway:\n  url: https://g\n",
        )
        .expect("an anchored empty key should be ignored like any unknown key");

        assert_eq!(settings.relay.port, 4142);
        assert_eq!(settings.shadow.model, "m");
        assert_eq!(settings.gateway.url, "https://g");
    }
}
