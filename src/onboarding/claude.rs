use std::{env, fs, path::PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::{
    config::{load_settings, save_settings, RelaySettings},
    onboarding::token::ensure_token,
    system::home_dir,
};

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
}
