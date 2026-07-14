//! Fetches the admin-owned managed policy the Gateway serves at
//! `/relay/managed-config`. The policy is authored in a dedicated YAML file on
//! the Gateway; Relay pulls it to learn which Claude Code and Codex versions
//! and settings the fleet must run. Relay authenticates with the Gateway API
//! key it already holds, so only enrolled devices receive the policy.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{Map, Value};

const MANAGED_CONFIG_PATH: &str = "/relay/managed-config";
const DEFAULT_REGISTRY: &str = "npm";
const DEFAULT_CLAUDE_PACKAGE: &str = "@anthropic-ai/claude-code";
const DEFAULT_CODEX_PACKAGE: &str = "@openai/codex";

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ManagedConfig {
    #[serde(default)]
    pub claude_code: ClaudeCodePolicy,
    #[serde(default)]
    pub codex: CodexPolicy,
    pub policy_version: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ClaudeCodePolicy {
    pub channel: String,
    pub version: Option<String>,
    pub registry: String,
    pub package: String,
    pub model: Option<String>,
    pub managed_settings: Map<String, Value>,
}

impl Default for ClaudeCodePolicy {
    fn default() -> Self {
        Self {
            channel: "pinned".into(),
            version: None,
            registry: DEFAULT_REGISTRY.into(),
            package: DEFAULT_CLAUDE_PACKAGE.into(),
            model: None,
            managed_settings: Map::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct CodexPolicy {
    pub channel: String,
    pub version: Option<String>,
    pub registry: String,
    pub package: String,
    pub model: Option<String>,
}

impl Default for CodexPolicy {
    fn default() -> Self {
        Self {
            channel: "pinned".into(),
            version: None,
            registry: DEFAULT_REGISTRY.into(),
            package: DEFAULT_CODEX_PACKAGE.into(),
            model: None,
        }
    }
}

pub async fn fetch_managed_config(gateway_url: &str, api_key: &str) -> Result<ManagedConfig> {
    if api_key.trim().is_empty() {
        bail!("no Gateway API key configured; run `relay setup` first");
    }
    let url = format!("{}{MANAGED_CONFIG_PATH}", gateway_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(&url)
        .bearer_auth(api_key)
        .send()
        .await
        .with_context(|| format!("failed to reach {url}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("Gateway returned {status} for {url}: {body}");
    }

    response
        .json::<ManagedConfig>()
        .await
        .with_context(|| format!("failed to parse managed config from {url}"))
}
