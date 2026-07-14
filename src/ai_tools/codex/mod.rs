//! Codex settings writer. Mirrors `claude_cli`: reconciles the installed Codex
//! CLI to the admin-approved version and writes a managed `config.toml` that
//! routes Codex through the Gateway. Only this module knows Codex specifics;
//! version reconciliation and the shared token cache live outside it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{env, fs};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::{
    ai_tools::managed_config::CodexPolicy,
    ai_tools::version::{self, ToolReconcile},
    config::RelaySettings,
    system::home_dir,
};

const CODEX_MANAGED_SETTINGS_PATH_ENV: &str = "LITELLM_RELAY_CODEX_MANAGED_SETTINGS_PATH";
const GATEWAY_PROVIDER_ID: &str = "litellm-gateway";
pub const CODEX_TOOL_LABEL: &str = "Codex";

/// Reconciles this device's Codex against the approved policy: installs the
/// pinned version if it differs, then writes a managed `config.toml` pointing
/// Codex at the Gateway. Safe to run repeatedly; a matching version does no
/// install.
pub fn reconcile(policy: &CodexPolicy, settings: &RelaySettings) -> Result<ToolReconcile> {
    let state = version::reconcile(
        "codex",
        &policy.registry,
        &policy.package,
        policy.version.as_deref(),
    )?;
    let settings_path = codex_managed_settings_path();
    write_managed_settings(&settings_path, settings, policy)?;
    Ok(ToolReconcile {
        state,
        settings_path,
    })
}

fn codex_managed_settings_path() -> PathBuf {
    if let Ok(path) = env::var(CODEX_MANAGED_SETTINGS_PATH_ENV) {
        return PathBuf::from(path);
    }
    home_dir().join(".codex").join("config.toml")
}

#[derive(Serialize)]
struct CodexProvider {
    name: String,
    base_url: String,
    env_key: String,
    wire_api: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    http_headers: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct CodexManagedConfig {
    model: String,
    model_provider: String,
    model_providers: BTreeMap<String, CodexProvider>,
}

fn build_managed_config(settings: &RelaySettings, policy: &CodexPolicy) -> CodexManagedConfig {
    let http_headers = settings
        .codex
        .team
        .as_ref()
        .map(|team| BTreeMap::from([("x-litellm-team".to_string(), team.clone())]))
        .unwrap_or_default();

    let provider = CodexProvider {
        name: "LiteLLM Gateway".into(),
        base_url: format!("{}/v1", settings.gateway.url.trim_end_matches('/')),
        env_key: "LITELLM_GATEWAY_API_KEY".into(),
        wire_api: "chat".into(),
        http_headers,
    };

    let model = policy
        .model
        .clone()
        .unwrap_or_else(|| settings.codex.model.clone());

    CodexManagedConfig {
        model,
        model_provider: GATEWAY_PROVIDER_ID.into(),
        model_providers: BTreeMap::from([(GATEWAY_PROVIDER_ID.to_string(), provider)]),
    }
}

fn write_managed_settings(
    path: &Path,
    settings: &RelaySettings,
    policy: &CodexPolicy,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let config = build_managed_config(settings, policy);
    let serialized = toml::to_string_pretty(&config).context("failed to serialize Codex config")?;
    fs::write(path, serialized).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_point_provider_at_gateway_with_policy_model() {
        let mut settings = RelaySettings::default();
        settings.gateway.url = "https://gateway.example.com".into();
        let policy = CodexPolicy {
            version: Some("0.144.3".into()),
            model: Some("gpt-5-codex".into()),
            ..CodexPolicy::default()
        };

        let config = build_managed_config(&settings, &policy);

        assert_eq!(config.model, "gpt-5-codex");
        assert_eq!(config.model_provider, GATEWAY_PROVIDER_ID);
        let provider = &config.model_providers[GATEWAY_PROVIDER_ID];
        assert_eq!(provider.base_url, "https://gateway.example.com/v1");
        assert!(provider.http_headers.is_empty());
    }

    #[test]
    fn should_write_team_header_when_set() {
        let mut settings = RelaySettings::default();
        settings.codex.team = Some("engineering".into());
        let config = build_managed_config(&settings, &CodexPolicy::default());

        assert_eq!(
            config.model_providers[GATEWAY_PROVIDER_ID].http_headers["x-litellm-team"],
            "engineering"
        );
    }

    #[test]
    fn should_fall_back_to_settings_model_when_policy_has_none() {
        let mut settings = RelaySettings::default();
        settings.codex.model = "settings-codex-model".into();
        let config = build_managed_config(&settings, &CodexPolicy::default());
        assert_eq!(config.model, "settings-codex-model");
    }

    #[test]
    fn should_serialize_config_toml_with_provider_table() {
        let dir = std::env::temp_dir().join(format!("relay-codex-{}", uuid::Uuid::new_v4()));
        let path = dir.join("config.toml");
        let mut settings = RelaySettings::default();
        settings.gateway.url = "http://localhost:4000".into();

        write_managed_settings(&path, &settings, &CodexPolicy::default()).unwrap();

        let written = fs::read_to_string(&path).unwrap();
        assert!(written.contains("model_provider = \"litellm-gateway\""));
        assert!(written.contains("base_url = \"http://localhost:4000/v1\""));
        assert!(written.contains("[model_providers.litellm-gateway]"));

        fs::remove_dir_all(&dir).ok();
    }
}
