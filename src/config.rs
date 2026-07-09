use std::{env, path::PathBuf};

use crate::system::{env_bool, home_dir};

const DEFAULT_NOTION_DOMAINS: &[&str] = &[
    "notion.so",
    "notion.com",
    "api.notion.com",
    "www.notion.so",
    "app.notion.com",
];

const DEFAULT_AI_DOMAINS: &[&str] = &[
    "notion.so",
    "notion.com",
    "api.notion.com",
    "www.notion.so",
    "app.notion.com",
    "api.openai.com",
    "openai.com",
    "chatgpt.com",
    "api.anthropic.com",
    "anthropic.com",
    "claude.ai",
];

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
    pub fn from_env() -> Self {
        let relay_home = home_dir().join(".litellm-relay");
        Self {
            host: env::var("LITELLM_RELAY_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            port: env_parse("LITELLM_RELAY_PORT", 4142),
            log_path: env::var("LITELLM_RELAY_LOG_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| relay_home.join("relay.log.jsonl")),
            notion_domains: parse_domains(
                env::var("LITELLM_RELAY_NOTION_DOMAINS").ok(),
                DEFAULT_NOTION_DOMAINS,
            ),
            ai_domains: parse_domains(
                env::var("LITELLM_RELAY_AI_DOMAINS").ok(),
                DEFAULT_AI_DOMAINS,
            ),
            shadow_enabled: env_bool("LITELLM_RELAY_SHADOW_ENABLED", false),
            gateway_url: env::var("LITELLM_GATEWAY_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:4000".into()),
            gateway_api_key: env::var("LITELLM_GATEWAY_API_KEY")
                .ok()
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    env::var("LITELLM_API_KEY")
                        .ok()
                        .filter(|value| !value.is_empty())
                }),
            shadow_model: env::var("LITELLM_RELAY_SHADOW_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini".into()),
            shadow_min_interval_seconds: env_parse("LITELLM_RELAY_SHADOW_MIN_INTERVAL_SECONDS", 60),
            request_timeout_seconds: env_parse("LITELLM_RELAY_REQUEST_TIMEOUT_SECONDS", 10.0),
            payload_preview_bytes: env_parse("LITELLM_RELAY_PAYLOAD_PREVIEW_BYTES", 8192),
            payload_body_bytes: env_parse("LITELLM_RELAY_PAYLOAD_BODY_BYTES", 262_144),
            mitm_enabled: env_bool("LITELLM_RELAY_CAPTURE_PAYLOADS", true),
            mitm_ca_dir: env::var("LITELLM_RELAY_MITM_CA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| relay_home.join("mitm")),
        }
    }
}

pub fn normalize_host(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest).to_string();
    }
    host.split(':').next().unwrap_or(&host).to_string()
}

pub fn is_ai_host(host: &str, config: &RelayConfig) -> bool {
    is_domain_match(host, &config.ai_domains)
}

pub fn is_notion_host(host: &str, config: &RelayConfig) -> bool {
    is_domain_match(host, &config.notion_domains)
}

pub fn classify_host(host: &str, config: &RelayConfig) -> String {
    let normalized = normalize_host(host);
    if is_domain_match(&normalized, &config.notion_domains) {
        "notion".into()
    } else if is_domain_match(
        &normalized,
        &["api.openai.com", "openai.com", "chatgpt.com"].map(String::from),
    ) {
        "openai".into()
    } else if is_domain_match(
        &normalized,
        &["api.anthropic.com", "anthropic.com", "claude.ai"].map(String::from),
    ) {
        "anthropic".into()
    } else if is_ai_host(&normalized, config) {
        "ai".into()
    } else {
        "unknown".into()
    }
}

fn is_domain_match(host: &str, domains: &[String]) -> bool {
    let normalized = normalize_host(host);
    domains
        .iter()
        .any(|domain| normalized == *domain || normalized.ends_with(&format!(".{domain}")))
}

fn parse_domains(raw: Option<String>, default: &[&str]) -> Vec<String> {
    raw.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
            .collect()
    })
    .filter(|domains: &Vec<String>| !domains.is_empty())
    .unwrap_or_else(|| default.iter().map(|value| value.to_string()).collect())
}

fn env_parse<T>(name: &str, default: T) -> T
where
    T: std::str::FromStr,
{
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}
