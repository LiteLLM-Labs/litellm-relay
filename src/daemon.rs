use std::{sync::Arc, thread, time::Duration};

use serde_json::{json, Value};

use crate::{
    account::AccountService,
    broker::{caller::Peer, Broker, Handler, Refusal, Reply, Request},
    mcp::service::Switcher,
};

const ACCOUNT_PATIENCE: Duration = Duration::from_secs(3);

pub struct Daemon {
    broker: Arc<Broker>,
    account: Arc<AccountService>,
    account_patience: Duration,
}

impl Daemon {
    pub fn new(broker: Arc<Broker>, account: Arc<AccountService>) -> Self {
        Self {
            broker,
            account,
            account_patience: ACCOUNT_PATIENCE,
        }
    }

    fn refreshed(&self, reply: Reply) -> Reply {
        if !matches!(
            reply,
            Reply::Switched(_) | Reply::SignedIn { .. } | Reply::SignedOut
        ) {
            return reply;
        }
        let account = Arc::clone(&self.account);
        let (done, finished) = std::sync::mpsc::channel();
        thread::spawn(move || {
            account.recheck();
            let _ = done.send(());
        });
        if finished.recv_timeout(self.account_patience).is_err() {
            eprintln!(
                "daemon: the account re-read is still running after {} s; answering now and finishing it in the background",
                self.account_patience.as_secs_f32()
            );
        }
        reply
    }

    fn unknown_team(&self, team: &str) -> Option<Refusal> {
        let listed = |known: &[String]| known.iter().any(|id| id == team);
        if self
            .account
            .known_teams()
            .is_none_or(|known| listed(&known))
        {
            return None;
        }
        self.account.recheck();
        let known = self.account.known_teams()?;
        (!listed(&known)).then(|| {
            Refusal::UnknownTeam(format!(
                "no team {team:?} for this user; the known teams are {}",
                known.join(", ")
            ))
        })
    }
}

impl Switcher for Daemon {
    fn team_refusal(&self, team: &str) -> Option<Refusal> {
        self.broker
            .team_switch_refusal()
            .or_else(|| self.unknown_team(team))
    }

    fn environment_refusal(&self, name: &str) -> Option<Refusal> {
        self.broker.environment_switch_refusal(name)
    }

    fn switch_team(&self, team: &str) -> Reply {
        match self.team_refusal(team) {
            Some(refusal) => Reply::Refused(refusal),
            None => self.refreshed(self.broker.switch_team(team)),
        }
    }

    fn switch_environment(&self, name: &str) -> Reply {
        self.refreshed(self.broker.switch_environment(name))
    }

    fn selection(&self) -> Value {
        let account = self.account.status();
        let environments = self.account.environments();
        json!({
            "team": account["team"]["id"],
            "environment": environments["current"],
            "gateway_url": account["gateway"]["url"],
            "teams": account["teams"],
            "teams_error": account["teams_error"],
            "environments": environments["available"],
        })
    }
}

impl Handler for Daemon {
    fn handle(&self, request: Request, peer: Peer) -> Reply {
        match request {
            Request::SwitchTeam { team } => self.switch_team(&team),
            Request::SwitchEnvironment { environment } => self.switch_environment(&environment),
            Request::SignIn => self.refreshed(self.broker.sign_in()),
            Request::SignOut => self.refreshed(self.broker.sign_out()),
            Request::Recheck => Reply::Account(self.account.recheck()),
            other => self.broker.handle(other, peer),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use std::time::Instant;

    use super::*;
    use crate::{
        account::test_support::{account_on, FakeHttp},
        broker::{
            test_support::{idp_settings, static_key_settings, Rig, GATEWAY},
            SettingsSource,
        },
        config::EnvironmentEntry,
    };

    const USER_INFO: &str = r#"{"user_id":"user-1","user_info":{"user_email":"dev@example.com"},"teams":[{"team_id":"team-a","team_alias":"Team A"},{"team_id":"team-b","team_alias":null}]}"#;

    fn daemon_on(
        settings: crate::config::RelaySettings,
    ) -> (Rig, FakeHttp, Arc<AccountService>, Daemon) {
        let (rig, http, account) = account_on(settings);
        http.answer("/user/info?user_id=user-1", 200, USER_INFO);
        http.answer(
            "/key/info",
            200,
            r#"{"key":"sk-static","info":{"user_id":"user-1","team_id":null}}"#,
        );
        http.answer("/health/liveliness", 200, "\"I'm alive!\"");
        let daemon = Daemon::new(Arc::clone(&rig.broker), Arc::clone(&account));
        (rig, http, account, daemon)
    }

    fn count(http: &FakeHttp, route: &str) -> usize {
        http.calls_to(route).len()
    }

    #[test]
    fn should_refuse_a_team_missing_from_the_known_list_without_touching_the_broker() {
        let (rig, http, _account, daemon) = daemon_on(idp_settings(Some("team-a")));
        assert!(matches!(
            daemon.handle(Request::SignIn, rig.peer()),
            Reply::SignedIn { .. }
        ));
        assert_eq!(rig.keys.mints().len(), 1);

        let refused = daemon.handle(
            Request::SwitchTeam {
                team: "team-z".to_string(),
            },
            rig.peer(),
        );
        let Reply::Refused(refusal) = refused else {
            panic!("expected a refusal, got {refused:?}");
        };
        assert_eq!(refusal.reason(), "unknown_team");
        assert_eq!(
            refusal.message(),
            "no team \"team-z\" for this user; the known teams are team-a, team-b"
        );
        assert_eq!(rig.keys.mints().len(), 1);
        assert!(rig.keys.deletes().is_empty());
        assert_eq!(rig.broker.status().team.as_deref(), Some("team-a"));
        assert_eq!(count(&http, "/user/info?user_id=user-1"), 2);
        assert_eq!(
            daemon
                .team_refusal("team-z")
                .map(|refusal| refusal.reason()),
            Some("unknown_team")
        );
        assert_eq!(daemon.team_refusal("team-b"), None);
    }

    #[test]
    fn should_recheck_the_account_once_and_switch_to_a_team_the_gateway_just_added() {
        let (rig, http, _account, daemon) = daemon_on(idp_settings(Some("team-a")));
        assert!(matches!(
            daemon.handle(Request::SignIn, rig.peer()),
            Reply::SignedIn { .. }
        ));
        http.answer(
            "/user/info?user_id=user-1",
            200,
            r#"{"user_id":"user-1","teams":[{"team_id":"team-a"},{"team_id":"team-c"}]}"#,
        );

        let reply = daemon.handle(
            Request::SwitchTeam {
                team: "team-c".to_string(),
            },
            rig.peer(),
        );

        assert!(matches!(reply, Reply::Switched(_)), "{reply:?}");
        assert_eq!(rig.broker.status().team.as_deref(), Some("team-c"));
        assert_eq!(
            rig.keys.mints().last().map(|mint| mint.team.clone()),
            Some("team-c".to_string())
        );
    }

    #[test]
    fn should_refuse_a_switch_on_a_static_key_before_checking_the_team_list() {
        let (rig, http, account, daemon) = daemon_on(static_key_settings("sk-static"));
        account.recheck();
        assert_eq!(account.status()["teams"].as_array().map(Vec::len), Some(2));
        let version = rig.settings.version();
        for team in ["team-z", "team-b"] {
            let refused = daemon.handle(
                Request::SwitchTeam {
                    team: team.to_string(),
                },
                rig.peer(),
            );
            let Reply::Refused(refusal) = refused else {
                panic!("expected a refusal, got {refused:?}");
            };
            assert_eq!(refusal.reason(), "static_key");
            assert_eq!(
                daemon.team_refusal(team).map(|refusal| refusal.reason()),
                Some("static_key")
            );
        }
        let refused = daemon.handle(Request::SignOut, rig.peer());
        assert!(
            matches!(&refused, Reply::Refused(refusal) if refusal.reason() == "static_key"),
            "{refused:?}"
        );
        assert_eq!(rig.settings.version(), version);
        assert_eq!(count(&http, "/user/info?user_id=user-1"), 1);
    }

    #[test]
    fn should_answer_a_switch_within_the_patience_and_finish_the_account_re_read_behind_it() {
        let (rig, http, account, daemon) = daemon_on(idp_settings(Some("team-a")));
        let daemon = Daemon {
            account_patience: Duration::from_millis(100),
            ..daemon
        };
        daemon.handle(Request::SignIn, rig.peer());
        assert_eq!(account.status()["teams"].as_array().map(Vec::len), Some(2));
        http.delay("/user/info?user_id=user-1", Duration::from_millis(800));

        let started = Instant::now();
        let switched = daemon.handle(
            Request::SwitchTeam {
                team: "team-b".to_string(),
            },
            rig.peer(),
        );
        assert!(
            started.elapsed() < Duration::from_millis(600),
            "{:?}",
            started.elapsed()
        );
        assert!(matches!(switched, Reply::Switched(_)), "{switched:?}");
        let status = account.status();
        assert!(status["teams"].is_null(), "{status}");
        assert!(status["user"]["email"].is_null(), "{status}");
        assert_eq!(status["team"]["id"], "team-b");

        let deadline = Instant::now() + Duration::from_secs(5);
        while account.status()["teams"].is_null() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        let status = account.status();
        assert_eq!(status["teams"].as_array().map(Vec::len), Some(2));
        assert_eq!(status["user"]["email"], "dev@example.com");
        assert_eq!(count(&http, "/user/info?user_id=user-1"), 2);
    }

    #[test]
    fn should_clear_the_account_before_sign_out_answers() {
        let (rig, _http, account, daemon) = daemon_on(idp_settings(Some("team-a")));
        daemon.handle(Request::SignIn, rig.peer());
        assert_eq!(account.status()["teams"].as_array().map(Vec::len), Some(2));
        assert_eq!(account.status()["user"]["email"], "dev@example.com");

        assert_eq!(
            daemon.handle(Request::SignOut, rig.peer()),
            Reply::SignedOut
        );
        let status = account.status();
        assert!(status["teams"].is_null(), "{status}");
        assert!(status["user"]["email"].is_null(), "{status}");
        assert!(status["team"]["spend"].is_null(), "{status}");
    }

    #[test]
    fn should_forward_a_known_team_and_poll_the_account_at_once() {
        let (rig, http, account, daemon) = daemon_on(idp_settings(Some("team-a")));
        assert!(matches!(
            daemon.handle(Request::SignIn, rig.peer()),
            Reply::SignedIn { .. }
        ));
        assert_eq!(count(&http, "/health/liveliness"), 1);

        let switched = daemon.handle(
            Request::SwitchTeam {
                team: "team-b".to_string(),
            },
            rig.peer(),
        );
        let Reply::Switched(switched) = switched else {
            panic!("expected a switch, got {switched:?}");
        };
        assert_eq!(switched.team.as_deref(), Some("team-b"));
        assert_eq!(rig.keys.mints().len(), 2);
        assert_eq!(count(&http, "/user/info?user_id=user-1"), 2);
        assert_eq!(count(&http, "/team/info?team_id=team-b"), 1);
        assert_eq!(count(&http, "/health/liveliness"), 2);
        assert_eq!(account.status()["team"]["id"], "team-b");
        account.poll();
        assert_eq!(count(&http, "/user/info?user_id=user-1"), 2);
        assert_eq!(count(&http, "/health/liveliness"), 2);
    }

    #[test]
    fn should_let_the_gateway_decide_a_team_while_the_list_is_unknown() {
        let (rig, http, _account, daemon) = daemon_on(idp_settings(Some("team-a")));
        http.fail("/user/info?user_id=user-1", "connection refused");
        assert!(matches!(
            daemon.handle(Request::SignIn, rig.peer()),
            Reply::SignedIn { .. }
        ));
        let switched = daemon.handle(
            Request::SwitchTeam {
                team: "team-b".to_string(),
            },
            rig.peer(),
        );
        assert!(matches!(switched, Reply::Switched(_)), "{switched:?}");
        assert_eq!(rig.broker.status().team.as_deref(), Some("team-b"));
    }

    #[test]
    fn should_forward_an_environment_switch_and_poll_the_new_gateway_at_once() {
        let mut settings = idp_settings(Some("team-a"));
        settings.environments = vec![
            EnvironmentEntry {
                name: "dev".to_string(),
                url: GATEWAY.to_string(),
                team: None,
            },
            EnvironmentEntry {
                name: "uat".to_string(),
                url: "https://uat.example.com".to_string(),
                team: None,
            },
        ];
        let (rig, http, account, daemon) = daemon_on(settings);
        daemon.handle(Request::SignIn, rig.peer());
        assert_eq!(account.status()["gateway"]["url"], GATEWAY);

        let unknown = daemon.handle(
            Request::SwitchEnvironment {
                environment: "prod".to_string(),
            },
            rig.peer(),
        );
        assert!(
            matches!(unknown, Reply::Refused(Refusal::UnknownEnvironment(_))),
            "{unknown:?}"
        );
        assert!(matches!(
            daemon.environment_refusal("prod"),
            Some(Refusal::UnknownEnvironment(_))
        ));
        assert_eq!(daemon.environment_refusal("uat"), None);

        let switched = daemon.handle(
            Request::SwitchEnvironment {
                environment: "uat".to_string(),
            },
            rig.peer(),
        );
        assert!(matches!(switched, Reply::Switched(_)), "{switched:?}");
        let probes = http.calls_to("/health/liveliness");
        assert_eq!(probes.len(), 2);
        assert_eq!(probes[1].url, "https://uat.example.com/health/liveliness");
        assert_eq!(count(&http, "/user/info?user_id=user-1"), 2);
        let status = account.status();
        assert_eq!(status["gateway"]["url"], "https://uat.example.com");
        assert_eq!(status["gateway"]["reachable"], true);
        assert_eq!(status["user"]["email"], "dev@example.com");
    }

    #[test]
    fn should_answer_a_recheck_at_once_while_a_sign_in_is_pending() {
        let (rig, http, account, daemon) = daemon_on(idp_settings(Some("team-a")));
        rig.identity.set_sign_in_delay(Duration::from_millis(1500));
        let broker = Arc::clone(&rig.broker);
        let signing_in = std::thread::spawn(move || broker.sign_in());
        std::thread::sleep(Duration::from_millis(200));

        let started = Instant::now();
        let reply = daemon.handle(Request::Recheck, rig.peer());
        let waited = started.elapsed();
        assert!(
            waited < Duration::from_millis(600),
            "recheck waited {waited:?}"
        );
        let Reply::Account(status) = reply else {
            panic!("expected the account status, got {reply:?}");
        };
        assert_eq!(status["gateway"]["reachable"], true);
        assert_eq!(count(&http, "/user/info?user_id=user-1"), 0);

        assert!(matches!(
            signing_in.join().expect("sign in"),
            Reply::SignedIn { .. }
        ));
        account.recheck();
        assert_eq!(account.status()["user"]["email"], "dev@example.com");
    }

    #[test]
    fn should_answer_recheck_with_the_account_status_and_forward_the_rest() {
        let (rig, http, account, daemon) = daemon_on(idp_settings(Some("team-a")));
        daemon.handle(Request::SignIn, rig.peer());
        assert_eq!(account.status()["user"]["email"], "dev@example.com");
        assert_eq!(count(&http, "/user/info?user_id=user-1"), 1);
        let reply = daemon.handle(Request::Recheck, rig.peer());
        let Reply::Account(status) = reply else {
            panic!("expected the account status, got {reply:?}");
        };
        assert_eq!(count(&http, "/user/info?user_id=user-1"), 2);
        assert_eq!(count(&http, "/health/liveliness"), 2);
        assert_eq!(status["user"]["email"], "dev@example.com");
        assert_eq!(status["gateway"]["reachable"], true);
        assert_eq!(status, account.status());
        assert!(reply_ok(&daemon.handle(Request::Recheck, rig.peer())));

        assert!(matches!(
            daemon.handle(Request::Status, rig.peer()),
            Reply::Status(_)
        ));
        assert!(matches!(
            daemon.handle(Request::SignOut, rig.peer()),
            Reply::SignedOut
        ));
        assert!(!rig.broker.status().signed_in);
    }

    fn reply_ok(reply: &Reply) -> bool {
        reply.to_json()["ok"] == true
    }

    #[test]
    fn should_describe_the_selection_from_the_account_and_the_configured_environments() {
        let mut settings = idp_settings(Some("team-a"));
        settings.environments = vec![
            EnvironmentEntry {
                name: "dev".to_string(),
                url: GATEWAY.to_string(),
                team: None,
            },
            EnvironmentEntry {
                name: "uat".to_string(),
                url: "https://uat.example.com".to_string(),
                team: None,
            },
        ];
        let (rig, _http, _account, daemon) = daemon_on(settings);
        assert_eq!(
            daemon.selection(),
            json!({
                "team": "team-a",
                "environment": "dev",
                "gateway_url": GATEWAY,
                "teams": null,
                "teams_error": null,
                "environments": [
                    {"name": "dev", "url": GATEWAY},
                    {"name": "uat", "url": "https://uat.example.com"},
                ],
            })
        );

        daemon.handle(Request::SignIn, rig.peer());
        let selection = daemon.selection();
        assert_eq!(
            selection["teams"],
            json!([{"id": "team-a", "alias": "Team A"}, {"id": "team-b", "alias": null}])
        );
        assert!(matches!(
            Switcher::switch_team(&daemon, "team-b"),
            Reply::Switched(_)
        ));
        assert_eq!(daemon.selection()["team"], "team-b");
        assert!(!selection.to_string().contains("sk-"));
    }
}
