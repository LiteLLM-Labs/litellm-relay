use std::{fs, path::PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::{
    ai_tools::{
        credential::{bearer_plan, keep_key_for_broker, relay_executable, BearerPlan, Host},
        gateway_credential::print_bearer,
        launch_agent::{require_daemon, DaemonHost, Launchd},
    },
    config::{load_settings, save_settings, IdpOverrides, RelaySettings},
    system::home_dir,
};

/// How Claude Code should obtain the Gateway bearer credential. These are
/// mutually exclusive: a static key lives in the `ANTHROPIC_AUTH_TOKEN` env var,
/// while the helpers are a top-level `apiKeyHelper` command.
#[derive(Debug, PartialEq, Eq)]
enum Credential<'a> {
    /// `apiKeyHelper` running `relay credential`: the daemon's in-memory broker
    /// answers only a signed Claude Code process. The default on macOS.
    Broker { helper: String },
    /// `apiKeyHelper` running the on-disk token helper, for hosts without the
    /// parent-process signature check.
    TokenHelper,
    /// Static gateway key written to `ANTHROPIC_AUTH_TOKEN` for environments
    /// without an IdP.
    StaticKey(&'a str),
}

impl<'a> Credential<'a> {
    fn from_plan(plan: BearerPlan<'a>, exe: &str) -> Self {
        match plan {
            BearerPlan::Broker => Credential::Broker {
                helper: helper_command(exe, "credential"),
            },
            BearerPlan::LegacyHelper => Credential::TokenHelper,
            BearerPlan::StaticKey(key) => Credential::StaticKey(key),
        }
    }
}

/// Inputs for wiring Claude Code to route through the Gateway. Supplied by the
/// MDM package (Jamf/Intune) or interactively; any field left unset falls back
/// to the saved Relay config.
#[derive(Debug, Default)]
pub struct OnboardParams {
    pub gateway_url: Option<String>,
    pub team: Option<String>,
    pub model: Option<String>,
    /// Static gateway key fallback for environments without an IdP.
    pub api_key: Option<String>,
    pub idp: IdpOverrides,
    /// Suppress success output (used by autoconfigure, which prints its own
    /// summary). Standalone `relay onboard` leaves this false.
    pub quiet: bool,
}

/// Writes `~/.claude/settings.json` so `claude` sends requests to the Gateway
/// with the team header. By default the bearer source is Relay's token helper
/// (`apiKeyHelper`) and the developer signs in through their IdP on first use,
/// so no provider key ever touches the device. When a static Gateway key is
/// supplied (or configured with no IdP), it is written to `ANTHROPIC_AUTH_TOKEN`
/// instead.
pub fn onboard(params: OnboardParams) -> Result<()> {
    onboard_with(params, &Launchd)
}

fn onboard_with(params: OnboardParams, daemon: &dyn DaemonHost) -> Result<()> {
    let mut settings = load_settings()?;
    if let Some(gateway_url) = params.gateway_url {
        settings.gateway.url = gateway_url.trim_end_matches('/').to_string();
    }
    settings.idp.apply(&params.idp);
    if let Some(model) = params.model {
        settings.claude.model = model;
    }
    if params.team.is_some() {
        settings.claude.team = params.team;
    }

    let host = Host::current();
    let explicit_key = params.api_key.filter(|key| !key.trim().is_empty());
    keep_key_for_broker(&mut settings, host, explicit_key.as_deref());
    // A static key resolves from --api-key, or from a saved gateway key when no
    // IdP is configured. A configured IdP is always preferred.
    let static_key = explicit_key.as_deref().or_else(|| {
        if !settings.idp.is_configured() {
            settings
                .gateway
                .api_key
                .as_deref()
                .filter(|key| !key.trim().is_empty())
        } else {
            None
        }
    });

    let credential = match bearer_plan(host, settings.idp.is_configured(), static_key) {
        Some(plan) => Credential::from_plan(plan, &relay_executable()?),
        None => bail!(
            "onboarding requires an IdP ({}) or a static Gateway key (--api-key or gateway.api_key)",
            settings.idp.setup_hint()
        ),
    };

    let settings_path = write_claude_settings(&settings, &credential)?;
    save_settings(&settings)?;
    if let Credential::Broker { .. } = credential {
        require_daemon(daemon, params.quiet)?;
    }

    if !params.quiet {
        println!("Claude Code is wired to {}", settings.gateway.url);
        if let Some(team) = &settings.claude.team {
            println!("Team header: x-litellm-team: {team}");
        }
        println!("Wrote {}", settings_path.display());
        match &credential {
            Credential::StaticKey(_) => println!("Using a static gateway key."),
            Credential::Broker { .. } if settings.idp.is_configured() => {
                println!("Run `claude` and sign in through your browser on first use.");
            }
            Credential::Broker { .. } => {
                println!("Claude Code asks the Relay daemon for the configured gateway key.");
            }
            Credential::TokenHelper => {
                println!("Run `claude` and sign in through your browser on first use.");
            }
        }
    }
    Ok(())
}

/// Prints the Gateway credential on stdout for Claude Code's `apiKeyHelper`.
pub fn print_token() -> Result<()> {
    let settings = load_settings()?;
    print_bearer(&settings, settings.claude.team.as_deref())
}

fn claude_settings_path() -> PathBuf {
    home_dir().join(".claude").join("settings.json")
}

fn write_claude_settings(settings: &RelaySettings, credential: &Credential) -> Result<PathBuf> {
    let path = claude_settings_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let root = read_existing_settings(&path)?;
    let root = merge_claude_settings(root, settings, credential)?;

    let serialized = serde_json::to_string_pretty(&Value::Object(root))?;
    fs::write(&path, format!("{serialized}\n"))
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(path)
}

/// Merges Relay's managed keys into an existing `settings.json` object,
/// preserving all other keys. The credential modes are mutually exclusive:
/// StaticKey writes `ANTHROPIC_AUTH_TOKEN` and drops any `apiKeyHelper`, while
/// the helpers write `apiKeyHelper` and drop `ANTHROPIC_AUTH_TOKEN`.
fn merge_claude_settings(
    mut root: Map<String, Value>,
    settings: &RelaySettings,
    credential: &Credential,
) -> Result<Map<String, Value>> {
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
    match credential {
        Credential::StaticKey(key) => {
            env.insert(
                "ANTHROPIC_AUTH_TOKEN".into(),
                Value::String((*key).to_string()),
            );
        }
        Credential::TokenHelper | Credential::Broker { .. } => {
            env.remove("ANTHROPIC_AUTH_TOKEN");
        }
    }

    match credential {
        Credential::StaticKey(_) => {
            root.remove("apiKeyHelper");
        }
        Credential::TokenHelper => {
            root.insert(
                "apiKeyHelper".into(),
                Value::String(helper_command(&relay_executable()?, "claude-token")),
            );
        }
        Credential::Broker { helper } => {
            root.insert("apiKeyHelper".into(), Value::String(helper.clone()));
        }
    }

    Ok(root)
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

fn helper_command(exe: &str, subcommand: &str) -> String {
    format!("'{}' {subcommand}", exe.replace('\'', "'\\''"))
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
    fn should_quote_the_helper_command() {
        assert_eq!(
            helper_command("/opt/re'lay/litellm-relay", "credential"),
            "'/opt/re'\\''lay/litellm-relay' credential"
        );
        assert_eq!(
            helper_command("/opt/relay/litellm-relay", "claude-token"),
            "'/opt/relay/litellm-relay' claude-token"
        );
    }

    #[test]
    fn should_map_the_bearer_plan_onto_the_settings_shape() {
        let exe = "/opt/relay/litellm-relay";
        assert_eq!(
            Credential::from_plan(BearerPlan::Broker, exe),
            Credential::Broker {
                helper: "'/opt/relay/litellm-relay' credential".into()
            }
        );
        assert_eq!(
            Credential::from_plan(BearerPlan::LegacyHelper, exe),
            Credential::TokenHelper
        );
        assert_eq!(
            Credential::from_plan(BearerPlan::StaticKey("sk-1"), exe),
            Credential::StaticKey("sk-1")
        );
    }

    #[test]
    fn should_write_the_broker_helper_and_no_token_on_the_broker_path() {
        let settings = settings_with_team(Some("engineering"));
        let existing = serde_json::from_str::<Value>(
            r#"{"apiKeyHelper":"stale","env":{"ANTHROPIC_AUTH_TOKEN":"sk-stale","KEEP":"yes"}}"#,
        )
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
        let credential = Credential::Broker {
            helper: "'/opt/relay/litellm-relay' credential".into(),
        };

        let root = merge_claude_settings(existing, &settings, &credential).unwrap();

        assert_eq!(
            root["apiKeyHelper"],
            Value::String("'/opt/relay/litellm-relay' credential".into())
        );
        let env = root["env"].as_object().unwrap();
        assert!(
            !env.contains_key("ANTHROPIC_AUTH_TOKEN"),
            "the broker path must leave no bearer in the settings file"
        );
        assert_eq!(env["KEEP"], Value::String("yes".into()));
        assert_eq!(
            env["ANTHROPIC_CUSTOM_HEADERS"],
            Value::String("x-litellm-team: engineering".into())
        );
        assert!(!Value::Object(root).to_string().contains("sk-stale"));
    }

    fn settings_with_team(team: Option<&str>) -> RelaySettings {
        let mut settings = RelaySettings::default();
        settings.gateway.url = "https://gateway.example.com".into();
        settings.claude.model = "claude-sonnet-4-5".into();
        settings.claude.team = team.map(str::to_string);
        settings
    }

    #[test]
    fn should_write_static_auth_token_and_drop_api_key_helper() {
        let settings = settings_with_team(Some("engineering"));
        let existing = serde_json::from_str::<Value>(r#"{"apiKeyHelper":"stale","keep":true}"#)
            .unwrap()
            .as_object()
            .unwrap()
            .clone();

        let root =
            merge_claude_settings(existing, &settings, &Credential::StaticKey("sk-static-123"))
                .unwrap();

        assert_eq!(
            root["env"]["ANTHROPIC_BASE_URL"],
            Value::String("https://gateway.example.com".into())
        );
        assert_eq!(
            root["env"]["ANTHROPIC_MODEL"],
            Value::String("claude-sonnet-4-5".into())
        );
        assert_eq!(
            root["env"]["ANTHROPIC_CUSTOM_HEADERS"],
            Value::String("x-litellm-team: engineering".into())
        );
        assert_eq!(
            root["env"]["ANTHROPIC_AUTH_TOKEN"],
            Value::String("sk-static-123".into())
        );
        assert!(
            !root.contains_key("apiKeyHelper"),
            "static key mode must remove any apiKeyHelper so the two do not conflict"
        );
        assert_eq!(root["keep"], Value::Bool(true));
    }

    #[test]
    fn should_write_api_key_helper_and_drop_static_auth_token() {
        let settings = settings_with_team(None);
        let existing =
            serde_json::from_str::<Value>(r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-stale"}}"#)
                .unwrap()
                .as_object()
                .unwrap()
                .clone();

        let root = merge_claude_settings(existing, &settings, &Credential::TokenHelper).unwrap();

        assert!(
            root["apiKeyHelper"]
                .as_str()
                .unwrap()
                .ends_with("claude-token"),
            "token helper mode must set apiKeyHelper"
        );
        assert!(
            root["env"]
                .as_object()
                .unwrap()
                .get("ANTHROPIC_AUTH_TOKEN")
                .is_none(),
            "token helper mode must remove a stale static ANTHROPIC_AUTH_TOKEN"
        );
        assert!(
            !root["env"]
                .as_object()
                .unwrap()
                .contains_key("ANTHROPIC_CUSTOM_HEADERS"),
            "no team means no custom headers"
        );
    }

    #[test]
    fn should_start_the_daemon_whenever_the_credential_helper_is_written() {
        use crate::ai_tools::launch_agent::test_support::{FakeHost, HOME_LOCK};
        use std::env;

        let _guard = HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = env::temp_dir().join(format!("relay-cc-daemon-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let old_home = env::var_os("HOME");
        env::set_var("HOME", &home);
        let params = || OnboardParams {
            gateway_url: Some("https://gw.corp".into()),
            api_key: Some("sk-saved".into()),
            quiet: true,
            ..OnboardParams::default()
        };

        let fresh = FakeHost::down();
        let first = onboard_with(params(), &fresh);
        let running = FakeHost::answering();
        let second = onboard_with(params(), &running);
        let broken = FakeHost::down().launchd_failing("denied");
        let third = onboard_with(params(), &broken);

        match old_home {
            Some(value) => env::set_var("HOME", value),
            None => env::remove_var("HOME"),
        }
        fs::remove_dir_all(&home).unwrap();
        first.unwrap();
        second.unwrap();
        match Host::current() {
            Host::MacOs => {
                assert_eq!(fresh.count("install_agent"), 1);
                assert_eq!(*running.calls.borrow(), vec!["answers"]);
                assert!(third
                    .unwrap_err()
                    .to_string()
                    .starts_with("could not start the Relay daemon"));
            }
            Host::Other => {
                assert!(fresh.calls.borrow().is_empty());
                assert!(running.calls.borrow().is_empty());
                third.unwrap();
            }
        }
    }
}
