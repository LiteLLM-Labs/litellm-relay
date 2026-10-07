use std::{
    cmp::Reverse,
    env,
    ffi::OsStr,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

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
    let claude_binary = claude_binary(env::var_os("PATH").as_deref(), &home_dir());
    onboard_with(params, &Launchd, claude_binary)
}

fn onboard_with(
    params: OnboardParams,
    daemon: &dyn DaemonHost,
    claude_binary: Option<PathBuf>,
) -> Result<()> {
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

    if let Credential::Broker { .. } = credential {
        refuse_node_script_claude_code(claude_binary.as_deref())?;
    }
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

/// PATH first, then the directories the installers use, since the PATH of a
/// LaunchAgent (the autoconfigure pass) carries neither Homebrew nor nvm.
fn claude_binary(path: Option<&OsStr>, home: &Path) -> Option<PathBuf> {
    let on_path = path.map(env::split_paths).into_iter().flatten();
    on_path
        .chain(install_dirs(home))
        .map(|dir| dir.join("claude"))
        .find(|candidate| candidate.is_file())
}

fn install_dirs(home: &Path) -> Vec<PathBuf> {
    let nvm_versions = fs::read_dir(home.join(".nvm/versions/node"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|version| version.path());
    [
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]
    .into_iter()
    .chain(nvm_bins_newest_first(nvm_versions))
    .collect()
}

/// nvm names a version directory `v<major>.<minor>.<patch>`; the newest one is
/// the likeliest default, so it is tried first and a name that is no version last.
fn nvm_bins_newest_first(versions: impl Iterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut bins: Vec<((u64, u64, u64), PathBuf)> = versions
        .map(|dir| {
            let name = dir.file_name().unwrap_or(OsStr::new(""));
            (node_version(name), dir.join("bin"))
        })
        .collect();
    bins.sort_by_key(|(version, _)| Reverse(*version));
    bins.into_iter().map(|(_, bin)| bin).collect()
}

fn node_version(name: &OsStr) -> (u64, u64, u64) {
    let mut parts = name
        .to_str()
        .unwrap_or("")
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

/// npm releases of Claude Code up to 2.1.110 ran `cli.js` as a script under
/// `node`, so no process in that chain carries Anthropic's signature and the
/// daemon would refuse every credential call; later npm releases hard-link the
/// native binary, which the daemon accepts like the installer's build.
fn refuse_node_script_claude_code(claude_binary: Option<&Path>) -> Result<()> {
    let Some(script) = claude_binary.filter(|path| runs_under_node(path)) else {
        return Ok(());
    };
    bail!(
        "Claude Code at {} is a script that runs under node, which carries no Anthropic code signature, so the Relay daemon would refuse its credential calls; install the native build (curl -fsSL https://claude.ai/install.sh | bash, or a current npm release, which ships it) and run this command again",
        script.display()
    )
}

fn runs_under_node(path: &Path) -> bool {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut head = [0u8; 128];
    let Ok(length) = fs::File::open(&resolved).and_then(|mut file| file.read(&mut head)) else {
        return false;
    };
    let first_line = String::from_utf8_lossy(&head[..length]);
    first_line.starts_with("#!")
        && first_line
            .lines()
            .next()
            .is_some_and(|line| line.contains("node"))
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
        let first = onboard_with(params(), &fresh, None);
        let running = FakeHost::answering();
        let second = onboard_with(params(), &running, None);
        let broken = FakeHost::down().launchd_failing("denied");
        let third = onboard_with(params(), &broken, None);

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
    #[test]
    fn should_tell_a_node_script_claude_code_from_a_native_binary() {
        let root = env::temp_dir().join(format!("relay-cc-channel-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let package = root.join("lib/node_modules/@anthropic-ai/claude-code");
        fs::create_dir_all(package.join("bin")).unwrap();
        let cli = package.join("cli.js");
        fs::write(&cli, "#!/usr/bin/env node\nconsole.log('claude')\n").unwrap();
        let wrapped_native = package.join("bin/claude.exe");
        fs::write(
            &wrapped_native,
            [0xcf, 0xfa, 0xed, 0xfe, 0x07, 0x00, 0x00, 0x01],
        )
        .unwrap();
        let old_link = root.join("claude-old");
        std::os::unix::fs::symlink(&cli, &old_link).unwrap();
        let new_link = root.join("claude-new");
        std::os::unix::fs::symlink(&wrapped_native, &new_link).unwrap();
        let native = root.join("claude-native");
        fs::write(&native, [0xcf, 0xfa, 0xed, 0xfe, 0x07, 0x00, 0x00, 0x01]).unwrap();
        let wrapper = root.join("claude-wrapper");
        fs::write(&wrapper, "#!/bin/sh\nexec /opt/claude/claude \"$@\"\n").unwrap();

        assert!(runs_under_node(&cli));
        assert!(runs_under_node(&old_link));
        assert!(!runs_under_node(&wrapped_native));
        assert!(!runs_under_node(&new_link));
        assert!(!runs_under_node(&native));
        assert!(!runs_under_node(&wrapper));
        assert!(!runs_under_node(&root.join("missing")));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn should_find_claude_code_where_its_installers_put_it_when_path_lacks_it() {
        let root = env::temp_dir().join(format!("relay-cc-lookup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let nvm_bin = home.join(".nvm/versions/node/v22.4.0/bin");
        let path_bin = root.join("path-bin");
        fs::create_dir_all(&nvm_bin).unwrap();
        fs::create_dir_all(&path_bin).unwrap();
        fs::create_dir_all(home.join(".local/bin")).unwrap();
        let path = env::join_paths([&path_bin]).unwrap();

        assert_eq!(claude_binary(Some(&path), &home), None);

        fs::write(nvm_bin.join("claude"), "#!/usr/bin/env node\n").unwrap();
        assert_eq!(
            claude_binary(Some(&path), &home),
            Some(nvm_bin.join("claude"))
        );
        assert_eq!(claude_binary(None, &home), Some(nvm_bin.join("claude")));

        let newest_nvm_bin = home.join(".nvm/versions/node/v24.19.0/bin");
        let other_nvm_bins = [
            home.join(".nvm/versions/node/v24.3.0/bin"),
            home.join(".nvm/versions/node/v9.0.0/bin"),
        ];
        for bin in other_nvm_bins.iter().chain([&newest_nvm_bin]) {
            fs::create_dir_all(bin).unwrap();
            fs::write(bin.join("claude"), [0xcf, 0xfa, 0xed, 0xfe]).unwrap();
        }
        assert_eq!(
            claude_binary(Some(&path), &home),
            Some(newest_nvm_bin.join("claude"))
        );

        fs::write(home.join(".local/bin/claude"), [0xcf, 0xfa, 0xed, 0xfe]).unwrap();
        assert_eq!(
            claude_binary(Some(&path), &home),
            Some(home.join(".local/bin/claude"))
        );

        fs::write(path_bin.join("claude"), [0xcf, 0xfa, 0xed, 0xfe]).unwrap();
        assert_eq!(
            claude_binary(Some(&path), &home),
            Some(path_bin.join("claude"))
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn should_try_nvm_versions_newest_first_after_the_fixed_install_dirs() {
        let root = env::temp_dir().join(format!("relay-nvm-order-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let home = root.join("home");
        let versions = home.join(".nvm/versions/node");
        let names = ["v9.0.0", "v22.4.0", "v24.3.0", "v24.19.0", "v10.24.1"];
        for name in names {
            fs::create_dir_all(versions.join(name).join("bin")).unwrap();
        }
        let newest_first = ["v24.19.0", "v24.3.0", "v22.4.0", "v10.24.1", "v9.0.0"]
            .map(|name| versions.join(name).join("bin"));

        let dirs = install_dirs(&home);
        assert_eq!(
            dirs[..3],
            [
                home.join(".local/bin"),
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/local/bin"),
            ]
        );
        assert_eq!(dirs[3..], newest_first);
        assert_eq!(
            nvm_bins_newest_first(names.into_iter().map(|name| versions.join(name))),
            newest_first
        );
        assert_eq!(node_version(OsStr::new("v24.19.0")), (24, 19, 0));
        assert_eq!(node_version(OsStr::new("v10.24.1")), (10, 24, 1));
        assert_eq!(node_version(OsStr::new("system")), (0, 0, 0));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn should_refuse_to_wire_a_node_script_claude_code_to_the_broker() {
        use crate::ai_tools::launch_agent::test_support::{FakeHost, HOME_LOCK};

        let _guard = HOME_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let home = env::temp_dir().join(format!("relay-cc-npm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        let script = home.join("claude");
        fs::write(&script, "#!/usr/bin/env node\n").unwrap();
        let old_home = env::var_os("HOME");
        env::set_var("HOME", &home);
        let host = FakeHost::down();
        let outcome = onboard_with(
            OnboardParams {
                gateway_url: Some("https://gw.corp".into()),
                api_key: Some("sk-saved".into()),
                quiet: true,
                ..OnboardParams::default()
            },
            &host,
            Some(script.clone()),
        );
        let settings_written = claude_settings_path().exists();

        match old_home {
            Some(value) => env::set_var("HOME", value),
            None => env::remove_var("HOME"),
        }
        fs::remove_dir_all(&home).unwrap();
        match Host::current() {
            Host::MacOs => {
                let error = outcome.unwrap_err().to_string();
                assert!(error.contains("runs under node"), "{error}");
                assert!(error.contains(&script.display().to_string()), "{error}");
                assert!(error.contains("claude.ai/install.sh"), "{error}");
                assert!(
                    !settings_written,
                    "nothing is written for a client the daemon would refuse"
                );
                assert!(
                    host.calls.borrow().is_empty(),
                    "the daemon is not started either"
                );
            }
            Host::Other => {
                outcome.unwrap();
                assert!(settings_written);
            }
        }
    }
}
