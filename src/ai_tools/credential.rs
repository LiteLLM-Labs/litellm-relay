//! The client side of the credential broker: `relay credential` prints the
//! Gateway bearer for an allowed client, `relay sign-in` and `relay sign-out`
//! drive the daemon's IdP session from a terminal, and the onboarding writers
//! ask here which bearer source a client file should name.

use std::{env, path::Path, process::ExitCode, time::Duration};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};

use crate::{
    ai_tools::idp::SIGN_IN_CEILING,
    broker::{Context as HelperContext, Request},
    config::RelaySettings,
};

pub const LAUNCH_AGENT_LABEL: &str = "ai.litellm.relay";
/// Set by Claude Desktop on every helper run; `background` and
/// `scheduled-task` mean nobody is watching for a browser.
const HELPER_CONTEXT_ENV: &str = "CLAUDE_HELPER_CONTEXT";
const REPLY_GRACE: Duration = Duration::from_secs(15);
const PROBE_PATIENCE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    MacOs,
    Other,
}

impl Host {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Host::MacOs
        } else {
            Host::Other
        }
    }
}

/// Which bearer source an onboarding writer names in a client file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearerPlan<'a> {
    /// `relay credential` over the broker socket (macOS, with or without an IdP).
    Broker,
    /// The legacy on-disk token helper, kept for hosts without the caller check.
    LegacyHelper,
    /// The key itself, written into the client file.
    StaticKey(&'a str),
}

pub fn bearer_plan(
    host: Host,
    idp_configured: bool,
    static_key: Option<&str>,
) -> Option<BearerPlan<'_>> {
    match (host, idp_configured, static_key) {
        (Host::MacOs, true, _) | (Host::MacOs, false, Some(_)) => Some(BearerPlan::Broker),
        (Host::Other, _, Some(key)) => Some(BearerPlan::StaticKey(key)),
        (Host::Other, true, None) => Some(BearerPlan::LegacyHelper),
        (_, false, None) => None,
    }
}

/// The broker serves whatever key Relay's own config holds, so a key passed on
/// the command line has to land there on a host where the broker is the bearer
/// source. With an IdP configured the broker serves the sign-in instead and
/// the key is not used, which the operator is told once.
pub fn keep_key_for_broker(settings: &mut RelaySettings, host: Host, explicit_key: Option<&str>) {
    let Some(key) = explicit_key.filter(|_| host == Host::MacOs) else {
        return;
    };
    if settings.idp.is_configured() {
        eprintln!(
            "--api-key is not used on macOS while an IdP is configured; the Relay daemon serves the sign-in instead"
        );
        return;
    }
    settings.gateway.enroll_if_changed(key.to_string());
}

/// The path client files call the helper by: the running executable, resolved
/// through any symlink so the path does not depend on the caller's PATH or on
/// a link that a later install may point elsewhere.
pub fn relay_executable() -> Result<String> {
    let exe = env::current_exe().context("failed to resolve the Relay executable path")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    exe.to_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Relay executable path is not valid UTF-8"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Token(String),
    Done(String),
    Json(Value),
    Refused(Value),
    Failed(String),
}

pub fn run_credential(audience: Audience) -> ExitCode {
    let context = helper_context(env::var(HELPER_CONTEXT_ENV).ok().as_deref());
    finish(ask(credential_request(audience, context)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    Gateway,
    LocalProxy,
}

pub(crate) fn credential_request(audience: Audience, context: HelperContext) -> Request {
    match audience {
        Audience::Gateway => Request::Credential { context },
        Audience::LocalProxy => Request::ProxyCredential { context },
    }
}

pub const PROXY_FLAG: &str = "--proxy";

pub fn local_proxy_url(settings: &RelaySettings) -> String {
    format!("http://127.0.0.1:{}", settings.relay.port)
}

pub fn run_sign_in() -> ExitCode {
    finish(ask(Request::SignIn))
}

pub fn run_sign_out() -> ExitCode {
    finish(ask(Request::SignOut))
}

pub fn run_switch_team(team: &str) -> ExitCode {
    finish(ask(Request::SwitchTeam {
        team: team.to_string(),
    }))
}

pub fn run_switch_environment(environment: &str) -> ExitCode {
    finish(ask(Request::SwitchEnvironment {
        environment: environment.to_string(),
    }))
}

pub fn run_recheck() -> ExitCode {
    finish(ask(Request::Recheck))
}

fn finish(outcome: Outcome) -> ExitCode {
    match outcome {
        Outcome::Token(token) => {
            println!("{token}");
            ExitCode::SUCCESS
        }
        Outcome::Done(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Outcome::Json(body) => {
            println!("{body}");
            ExitCode::SUCCESS
        }
        Outcome::Refused(body) => {
            eprintln!("{body}");
            ExitCode::FAILURE
        }
        Outcome::Failed(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn helper_context(value: Option<&str>) -> HelperContext {
    match value.map(str::trim) {
        Some("background") | Some("scheduled-task") => HelperContext::NonInteractive,
        _ => HelperContext::Interactive,
    }
}

pub fn daemon_answers(socket: &Path) -> bool {
    !matches!(
        transport::exchange(socket, &Request::Status, PROBE_PATIENCE),
        Err(transport::Failure::NoDaemon)
    )
}

fn ask(request: Request) -> Outcome {
    let path = crate::broker::socket_path();
    match transport::exchange(&path, &request, SIGN_IN_CEILING + REPLY_GRACE) {
        Ok(reply) => interpret(&request, &reply),
        Err(failure) => unreachable_daemon(&request, &path, failure),
    }
}

fn unreachable_daemon(request: &Request, path: &Path, failure: transport::Failure) -> Outcome {
    let message = match failure {
        transport::Failure::NoDaemon => format!(
            "the Relay daemon is not running (no socket at {}); run `relay autoconfigure` or your \
             onboard command again to start the {LAUNCH_AGENT_LABEL} LaunchAgent, or run `relay \
             serve` in a terminal",
            path.display()
        ),
        transport::Failure::Broken(message) => message,
    };
    match answers_json(request) {
        true => Outcome::Refused(json!({ "refused": "daemon_unavailable", "message": message })),
        false => Outcome::Failed(format!("relay credential: {message}")),
    }
}

fn answers_json(request: &Request) -> bool {
    matches!(
        request,
        Request::SwitchTeam { .. } | Request::SwitchEnvironment { .. } | Request::Recheck
    )
}

fn without_ok(reply: &Value) -> Value {
    match reply {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(key, _)| key.as_str() != "ok")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub(crate) fn interpret(request: &Request, reply: &Value) -> Outcome {
    if reply["ok"] != Value::Bool(true) {
        let reason = reply["reason"].as_str().unwrap_or("error");
        let message = reply["message"]
            .as_str()
            .unwrap_or("the daemon answered with an error");
        return match answers_json(request) {
            true => Outcome::Refused(json!({ "refused": reason, "message": message })),
            false => Outcome::Failed(format!("relay credential: {reason}: {message}")),
        };
    }
    match request {
        Request::Credential { .. } | Request::ProxyCredential { .. } => {
            match reply["token"].as_str() {
                Some(token) if !token.is_empty() => Outcome::Token(token.to_string()),
                _ => Outcome::Failed(
                    "relay credential: the daemon answered without a token".to_string(),
                ),
            }
        }
        Request::SignIn => Outcome::Done(match reply["user_id"].as_str() {
            Some(user) => format!("Signed in as {user}."),
            None => "Signed in.".to_string(),
        }),
        Request::SignOut => {
            Outcome::Done("Signed out; the session and its key are gone.".to_string())
        }
        Request::Status => Outcome::Done(reply.to_string()),
        Request::SwitchTeam { .. } | Request::SwitchEnvironment { .. } | Request::Recheck => {
            Outcome::Json(without_ok(reply))
        }
    }
}

#[cfg(unix)]
mod transport {
    use super::Request;
    use serde_json::Value;
    use std::{
        io::{BufRead, BufReader, ErrorKind, Write},
        os::unix::net::UnixStream,
        path::Path,
        time::Duration,
    };

    pub(super) enum Failure {
        NoDaemon,
        Broken(String),
    }

    pub(super) fn exchange(
        path: &Path,
        request: &Request,
        patience: Duration,
    ) -> Result<Value, Failure> {
        let mut stream = UnixStream::connect(path).map_err(|error| match error.kind() {
            ErrorKind::NotFound | ErrorKind::ConnectionRefused => Failure::NoDaemon,
            _ => Failure::Broken(format!("could not reach the daemon socket: {error}")),
        })?;
        stream
            .set_read_timeout(Some(patience))
            .map_err(|error| Failure::Broken(error.to_string()))?;
        let line =
            serde_json::to_string(request).map_err(|error| Failure::Broken(error.to_string()))?;
        stream
            .write_all(format!("{line}\n").as_bytes())
            .map_err(|error| Failure::Broken(format!("could not send the request: {error}")))?;
        let mut reply = String::new();
        BufReader::new(&stream)
            .read_line(&mut reply)
            .map_err(|error| Failure::Broken(format!("no answer from the daemon: {error}")))?;
        serde_json::from_str(reply.trim()).map_err(|_| {
            Failure::Broken("the daemon answered with something other than JSON".to_string())
        })
    }
}

#[cfg(not(unix))]
mod transport {
    use super::Request;
    use serde_json::Value;
    use std::{path::Path, time::Duration};

    pub(super) enum Failure {
        NoDaemon,
        Broken(String),
    }

    pub(super) fn exchange(
        _path: &Path,
        _request: &Request,
        _patience: Duration,
    ) -> Result<Value, Failure> {
        Err(Failure::Broken(
            "the credential broker needs a Unix socket, which this platform has none of"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_print_a_bare_token_only_for_an_ok_credential_reply() {
        let request = Request::Credential {
            context: HelperContext::Interactive,
        };
        assert_eq!(
            interpret(
                &request,
                &json!({"ok": true, "token": "sk-abc", "source": "minted_key"})
            ),
            Outcome::Token("sk-abc".into())
        );
        assert_eq!(
            interpret(
                &request,
                &json!({"ok": false, "reason": "caller_refused", "message": "no allowed client"})
            ),
            Outcome::Failed("relay credential: caller_refused: no allowed client".into())
        );
        assert!(matches!(
            interpret(
                &Request::Credential {
                    context: HelperContext::Interactive
                },
                &json!({"ok": true})
            ),
            Outcome::Failed(_)
        ));
    }

    #[test]
    fn should_describe_sign_in_and_sign_out_without_a_token() {
        assert_eq!(
            interpret(
                &Request::SignIn,
                &json!({"ok": true, "signed_in": true, "user_id": "alice"})
            ),
            Outcome::Done("Signed in as alice.".into())
        );
        assert!(matches!(
            interpret(&Request::SignOut, &json!({"ok": true, "signed_in": false})),
            Outcome::Done(_)
        ));
    }

    #[test]
    fn should_print_a_switch_reply_as_its_fields_and_a_refusal_as_refused_json() {
        let request = Request::SwitchTeam {
            team: "eng".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"op": "switch_team", "team": "eng"})
        );
        assert_eq!(
            interpret(
                &request,
                &json!({
                    "ok": true,
                    "team": "eng",
                    "environment": "uat",
                    "gateway_url": "https://uat.example.com",
                    "key_expires_at": "2027-01-15T08:00:00Z",
                    "source": "minted_key"
                })
            ),
            Outcome::Json(json!({
                "team": "eng",
                "environment": "uat",
                "gateway_url": "https://uat.example.com",
                "key_expires_at": "2027-01-15T08:00:00Z",
                "source": "minted_key"
            }))
        );
        let environment = Request::SwitchEnvironment {
            environment: "qa".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&environment).unwrap(),
            json!({"op": "switch_environment", "environment": "qa"})
        );
        assert_eq!(
            interpret(
                &environment,
                &json!({
                    "ok": false,
                    "reason": "unknown_environment",
                    "message": "no environment named \"qa\"; configured: prod, uat"
                })
            ),
            Outcome::Refused(json!({
                "refused": "unknown_environment",
                "message": "no environment named \"qa\"; configured: prod, uat"
            }))
        );
    }

    #[test]
    fn should_print_the_account_block_for_a_recheck_reply() {
        assert_eq!(
            serde_json::to_value(Request::Recheck).unwrap(),
            json!({"op": "recheck"})
        );
        assert_eq!(
            interpret(
                &Request::Recheck,
                &json!({"ok": true, "teams": [{"id": "team-a", "alias": null}], "gateway": {"reachable": true}})
            ),
            Outcome::Json(
                json!({"teams": [{"id": "team-a", "alias": null}], "gateway": {"reachable": true}})
            )
        );
        assert_eq!(
            interpret(
                &Request::Recheck,
                &json!({"ok": false, "reason": "bad_request", "message": "nope"})
            ),
            Outcome::Refused(json!({"refused": "bad_request", "message": "nope"}))
        );
        assert_eq!(
            unreachable_daemon(
                &Request::Recheck,
                Path::new("/nonexistent/broker.sock"),
                transport::Failure::Broken("no answer from the daemon".to_string()),
            ),
            Outcome::Refused(
                json!({"refused": "daemon_unavailable", "message": "no answer from the daemon"})
            )
        );
    }

    #[test]
    fn should_answer_a_missing_daemon_as_refused_json_only_for_a_switch() {
        let path = Path::new("/nonexistent/broker.sock");
        let switch = unreachable_daemon(
            &Request::SwitchTeam {
                team: "eng".to_string(),
            },
            path,
            transport::Failure::NoDaemon,
        );
        let Outcome::Refused(body) = switch else {
            panic!("expected refused JSON, got {switch:?}");
        };
        assert_eq!(body["refused"], "daemon_unavailable");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("relay serve")),
            "{body}"
        );
        let broken = unreachable_daemon(
            &Request::SwitchEnvironment {
                environment: "uat".to_string(),
            },
            path,
            transport::Failure::Broken("no answer from the daemon".to_string()),
        );
        assert_eq!(
            broken,
            Outcome::Refused(
                json!({"refused": "daemon_unavailable", "message": "no answer from the daemon"})
            )
        );
        assert_eq!(
            unreachable_daemon(&Request::SignIn, path, transport::Failure::NoDaemon),
            Outcome::Failed(format!(
                "relay credential: the Relay daemon is not running (no socket at {}); run `relay \
                 autoconfigure` or your onboard command again to start the {LAUNCH_AGENT_LABEL} \
                 LaunchAgent, or run `relay serve` in a terminal",
                path.display()
            ))
        );
    }

    #[test]
    fn should_map_the_desktop_helper_context_to_non_interactive_only_when_unattended() {
        assert_eq!(
            helper_context(Some("background")),
            HelperContext::NonInteractive
        );
        assert_eq!(
            helper_context(Some("scheduled-task")),
            HelperContext::NonInteractive
        );
        assert_eq!(
            helper_context(Some("interactive")),
            HelperContext::Interactive
        );
        assert_eq!(
            helper_context(Some("mid-session-refresh")),
            HelperContext::Interactive
        );
        assert_eq!(
            helper_context(Some("setup-test")),
            HelperContext::Interactive
        );
        assert_eq!(helper_context(None), HelperContext::Interactive);
    }

    #[test]
    fn should_keep_a_passed_key_in_the_relay_config_only_where_the_broker_serves_it() {
        let mut no_idp = RelaySettings::default();
        keep_key_for_broker(&mut no_idp, Host::MacOs, Some("sk-passed"));
        assert_eq!(no_idp.gateway.api_key.as_deref(), Some("sk-passed"));

        let mut other_host = RelaySettings::default();
        keep_key_for_broker(&mut other_host, Host::Other, Some("sk-passed"));
        assert_eq!(other_host.gateway.api_key, None);

        let mut with_idp = RelaySettings::default();
        with_idp.idp.issuer = "https://login.example.com/v2.0".into();
        with_idp.idp.client_id = "relay-client".into();
        keep_key_for_broker(&mut with_idp, Host::MacOs, Some("sk-passed"));
        assert_eq!(with_idp.gateway.api_key, None);

        let mut nothing_passed = RelaySettings::default();
        keep_key_for_broker(&mut nothing_passed, Host::MacOs, None);
        assert_eq!(nothing_passed.gateway.api_key, None);
    }

    #[cfg(unix)]
    #[test]
    fn should_see_a_daemon_only_while_something_listens_on_the_socket() {
        use std::{fs, io::Write, os::unix::net::UnixListener, thread};

        let dir = env::temp_dir().join(format!("relay-probe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("b.sock");
        assert!(!daemon_answers(&socket));

        let listener = UnixListener::bind(&socket).unwrap();
        let daemon = thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            peer.write_all(b"{\"ok\":false,\"reason\":\"caller_refused\"}\n")
                .unwrap();
        });
        assert!(daemon_answers(&socket));
        daemon.join().unwrap();

        assert!(socket.exists());
        assert!(!daemon_answers(&socket));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn should_pick_the_broker_on_macos_and_the_legacy_sources_elsewhere() {
        assert_eq!(
            bearer_plan(Host::MacOs, true, None),
            Some(BearerPlan::Broker)
        );
        assert_eq!(
            bearer_plan(Host::MacOs, true, Some("sk-1")),
            Some(BearerPlan::Broker)
        );
        assert_eq!(
            bearer_plan(Host::MacOs, false, Some("sk-1")),
            Some(BearerPlan::Broker)
        );
        assert_eq!(bearer_plan(Host::MacOs, false, None), None);
        assert_eq!(
            bearer_plan(Host::Other, true, None),
            Some(BearerPlan::LegacyHelper)
        );
        assert_eq!(
            bearer_plan(Host::Other, true, Some("sk-1")),
            Some(BearerPlan::StaticKey("sk-1"))
        );
        assert_eq!(
            bearer_plan(Host::Other, false, Some("sk-1")),
            Some(BearerPlan::StaticKey("sk-1"))
        );
        assert_eq!(bearer_plan(Host::Other, false, None), None);
    }

    #[test]
    fn should_ask_for_the_proxy_token_only_when_the_local_proxy_is_the_audience() {
        assert_eq!(
            credential_request(Audience::Gateway, HelperContext::Interactive),
            Request::Credential {
                context: HelperContext::Interactive
            }
        );
        let request = credential_request(Audience::LocalProxy, HelperContext::NonInteractive);
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"op": "proxy_credential", "context": "non_interactive"})
        );
        assert_eq!(
            interpret(
                &request,
                &json!({"ok": true, "token": "relay-proxy-abc", "source": "proxy_token"})
            ),
            Outcome::Token("relay-proxy-abc".to_string())
        );
    }

    #[test]
    fn should_name_loopback_and_the_relay_port_as_the_local_proxy_url() {
        let mut settings = RelaySettings::default();
        settings.relay.host = "0.0.0.0".into();
        settings.relay.port = 4199;
        assert_eq!(local_proxy_url(&settings), "http://127.0.0.1:4199");
    }
}
