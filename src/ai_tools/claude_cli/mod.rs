use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::{
    ai_tools::managed_config::ClaudeCodePolicy,
    ai_tools::token::ensure_token,
    ai_tools::version::{self, ToolReconcile},
    config::{load_settings, save_settings, RelaySettings},
    system::home_dir,
};

const CLAUDE_MANAGED_SETTINGS_PATH_ENV: &str = "LITELLM_RELAY_CLAUDE_MANAGED_SETTINGS_PATH";
pub const CLAUDE_TOOL_LABEL: &str = "Claude Code";

/// Inputs for wiring Claude Code to route through the Gateway. Supplied by the
/// MDM package (Jamf/Intune) or interactively; any field left unset falls back
/// to the saved Relay config.
#[derive(Debug, Default)]
pub struct OnboardParams {
    pub gateway_url: Option<String>,
    pub authorize_url: Option<String>,
    pub team: Option<String>,
    pub model: Option<String>,
}

/// Writes `~/.claude/settings.json` so `claude` sends requests to the Gateway
/// with the team header, using Relay's token helper as the bearer source. The
/// developer runs `claude` and signs in through their IdP on first use; no
/// provider key ever touches the device.
pub fn onboard(params: OnboardParams) -> Result<()> {
    let mut settings = load_settings()?;
    if let Some(gateway_url) = params.gateway_url {
        settings.gateway.url = gateway_url.trim_end_matches('/').to_string();
    }
    if let Some(authorize_url) = params.authorize_url {
        settings.idp.authorize_url = authorize_url;
    }
    if let Some(model) = params.model {
        settings.claude.model = model;
    }
    if params.team.is_some() {
        settings.claude.team = params.team;
    }

    if settings.idp.authorize_url.trim().is_empty() {
        bail!("onboarding requires an IdP authorize URL (--authorize-url or idp.authorize_url)");
    }

    let settings_path = write_claude_settings(&settings)?;
    save_settings(&settings)?;

    println!("Claude Code is wired to {}", settings.gateway.url);
    if let Some(team) = &settings.claude.team {
        println!("Team header: x-litellm-team: {team}");
    }
    println!("Wrote {}", settings_path.display());
    println!("Run `claude` and sign in through your browser on first use.");
    Ok(())
}

/// Prints a valid IdP bearer token on stdout for Claude Code's `apiKeyHelper`.
pub fn print_token() -> Result<()> {
    let settings = load_settings()?;
    let token = ensure_token(&settings.idp.authorize_url)?;
    println!("{token}");
    Ok(())
}

/// Reconciles this device's Claude Code against the approved policy: installs
/// the pinned version if it differs, then writes Claude Code enterprise managed
/// settings so the model and Gateway wiring cannot be overridden locally. Safe
/// to run repeatedly; a matching version does no install.
pub fn reconcile(policy: &ClaudeCodePolicy, settings: &RelaySettings) -> Result<ToolReconcile> {
    let state = version::reconcile(
        "claude",
        &policy.registry,
        &policy.package,
        policy.version.as_deref(),
    )?;
    let settings_path = claude_managed_settings_path();
    write_managed_settings(&settings_path, settings, policy)?;
    Ok(ToolReconcile {
        state,
        settings_path,
    })
}

fn claude_managed_settings_path() -> PathBuf {
    if let Ok(path) = env::var(CLAUDE_MANAGED_SETTINGS_PATH_ENV) {
        return PathBuf::from(path);
    }
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json")
    }
    #[cfg(not(target_os = "macos"))]
    {
        PathBuf::from("/etc/claude-code/managed-settings.json")
    }
}

fn write_managed_settings(
    path: &Path,
    settings: &RelaySettings,
    policy: &ClaudeCodePolicy,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut root = Map::new();
    for (key, value) in &policy.managed_settings {
        root.insert(key.clone(), value.clone());
    }

    let mut env_map = match root.remove("env") {
        Some(Value::Object(existing)) => existing,
        _ => Map::new(),
    };
    env_map.insert(
        "ANTHROPIC_BASE_URL".into(),
        Value::String(settings.gateway.url.clone()),
    );
    let model = policy
        .model
        .clone()
        .unwrap_or_else(|| settings.claude.model.clone());
    env_map.insert("ANTHROPIC_MODEL".into(), Value::String(model));
    if let Some(team) = &settings.claude.team {
        env_map.insert(
            "ANTHROPIC_CUSTOM_HEADERS".into(),
            Value::String(format!("x-litellm-team: {team}")),
        );
    }
    root.insert("env".into(), Value::Object(env_map));

    let serialized = serde_json::to_string_pretty(&Value::Object(root))?;
    fs::write(path, format!("{serialized}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn claude_settings_path() -> PathBuf {
    home_dir().join(".claude").join("settings.json")
}

fn write_claude_settings(settings: &RelaySettings) -> Result<PathBuf> {
    let path = claude_settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut root = read_existing_settings(&path)?;
    let env = env_object(&mut root);
    env.insert(
        "ANTHROPIC_BASE_URL".into(),
        Value::String(settings.gateway.url.clone()),
    );
    env.insert(
        "ANTHROPIC_MODEL".into(),
        Value::String(settings.claude.model.clone()),
    );
    match &settings.claude.team {
        Some(team) => {
            env.insert(
                "ANTHROPIC_CUSTOM_HEADERS".into(),
                Value::String(format!("x-litellm-team: {team}")),
            );
        }
        None => {
            env.remove("ANTHROPIC_CUSTOM_HEADERS");
        }
    }

    root.insert(
        "apiKeyHelper".into(),
        Value::String(token_helper_command()?),
    );

    let serialized = serde_json::to_string_pretty(&Value::Object(root))?;
    fs::write(&path, format!("{serialized}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

fn read_existing_settings(path: &PathBuf) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    match serde_json::from_str::<Value>(&contents) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Ok(Map::new()),
    }
}

fn env_object(root: &mut Map<String, Value>) -> &mut Map<String, Value> {
    if !matches!(root.get("env"), Some(Value::Object(_))) {
        root.insert("env".into(), json!({}));
    }
    root.get_mut("env")
        .and_then(Value::as_object_mut)
        .expect("env was just inserted as an object")
}

fn token_helper_command() -> Result<String> {
    let exe = env::current_exe().context("failed to resolve the Relay executable path")?;
    let exe = exe
        .to_str()
        .ok_or_else(|| anyhow!("Relay executable path is not valid UTF-8"))?;
    Ok(format!("'{}' claude-token", exe.replace('\'', "'\\''")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_insert_managed_env_and_preserve_existing_keys() {
        let mut root =
            serde_json::from_str::<Value>(r#"{"env":{"EXISTING":"keep"},"otherTopLevel":true}"#)
                .unwrap()
                .as_object()
                .unwrap()
                .clone();

        let env = env_object(&mut root);
        env.insert(
            "ANTHROPIC_BASE_URL".into(),
            Value::String("http://gw".into()),
        );

        assert_eq!(root["env"]["EXISTING"], Value::String("keep".into()));
        assert_eq!(
            root["env"]["ANTHROPIC_BASE_URL"],
            Value::String("http://gw".into())
        );
        assert_eq!(root["otherTopLevel"], Value::Bool(true));
    }

    #[test]
    fn should_create_env_object_when_missing_or_wrong_type() {
        let mut root = serde_json::from_str::<Value>(r#"{"env":"not-an-object"}"#)
            .unwrap()
            .as_object()
            .unwrap()
            .clone();

        let env = env_object(&mut root);
        env.insert("K".into(), Value::String("v".into()));

        assert_eq!(root["env"]["K"], Value::String("v".into()));
    }

    #[test]
    fn should_quote_token_helper_command() {
        let command = token_helper_command().unwrap();
        assert!(command.ends_with("' claude-token"));
        assert!(command.starts_with('\''));
    }

    #[test]
    fn should_write_managed_env_from_policy_and_settings() {
        let dir = std::env::temp_dir().join(format!("relay-managed-{}", uuid::Uuid::new_v4()));
        let path = dir.join("managed-settings.json");

        let mut settings = RelaySettings::default();
        settings.gateway.url = "https://gateway.example.com".into();
        settings.claude.team = Some("engineering".into());
        settings.claude.model = "fallback-model".into();

        let policy = ClaudeCodePolicy {
            version: Some("1.2.3".into()),
            model: Some("claude-sonnet-4-5".into()),
            ..ClaudeCodePolicy::default()
        };

        write_managed_settings(&path, &settings, &policy).unwrap();

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            written["env"]["ANTHROPIC_BASE_URL"],
            "https://gateway.example.com"
        );
        assert_eq!(written["env"]["ANTHROPIC_MODEL"], "claude-sonnet-4-5");
        assert_eq!(
            written["env"]["ANTHROPIC_CUSTOM_HEADERS"],
            "x-litellm-team: engineering"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn should_fall_back_to_settings_model_when_policy_has_none() {
        let dir = std::env::temp_dir().join(format!("relay-managed-{}", uuid::Uuid::new_v4()));
        let path = dir.join("managed-settings.json");

        let mut settings = RelaySettings::default();
        settings.claude.model = "settings-model".into();
        let policy = ClaudeCodePolicy::default();

        write_managed_settings(&path, &settings, &policy).unwrap();

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["env"]["ANTHROPIC_MODEL"], "settings-model");

        fs::remove_dir_all(&dir).ok();
    }
}
