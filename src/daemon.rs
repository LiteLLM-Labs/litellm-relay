use std::sync::Arc;

use serde_json::{json, Value};

use crate::{
    account::AccountService,
    broker::{caller::Peer, Broker, Handler, Refusal, Reply, Request},
    mcp::service::Switcher,
};

pub struct Daemon {
    broker: Arc<Broker>,
    account: Arc<AccountService>,
}

impl Daemon {
    pub fn new(broker: Arc<Broker>, account: Arc<AccountService>) -> Self {
        Self { broker, account }
    }

    fn refreshed(&self, reply: Reply) -> Reply {
        if matches!(reply, Reply::Switched(_) | Reply::SignedIn { .. }) {
            self.account.recheck();
        }
        reply
    }
}

impl Switcher for Daemon {
    fn switch_team(&self, team: &str) -> Reply {
        if let Some(known) = self.account.known_teams() {
            if !known.iter().any(|id| id == team) {
                return Reply::Refused(Refusal::UnknownTeam(format!(
                    "no team {team:?} for this user; the known teams are {}",
                    known.join(", ")
                )));
            }
        }
        self.refreshed(self.broker.switch_team(team))
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
            Request::Recheck => Reply::Account(self.account.recheck()),
            other => self.broker.handle(other, peer),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        account::test_support::{account_on, FakeHttp},
        broker::test_support::{idp_settings, Rig, GATEWAY},
        config::EnvironmentEntry,
    };

    const USER_INFO: &str = r#"{"user_id":"user-1","user_info":{"user_email":"dev@example.com"},"teams":[{"team_id":"team-a","team_alias":"Team A"},{"team_id":"team-b","team_alias":null}]}"#;

    fn daemon_on(
        settings: crate::config::RelaySettings,
    ) -> (Rig, FakeHttp, Arc<AccountService>, Daemon) {
        let (rig, http, account) = account_on(settings);
        http.answer("/user/info", 200, USER_INFO);
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
        assert_eq!(count(&http, "/user/info"), 1);
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
        assert_eq!(count(&http, "/user/info"), 2);
        assert_eq!(count(&http, "/team/info?team_id=team-b"), 1);
        assert_eq!(count(&http, "/health/liveliness"), 2);
        assert_eq!(account.status()["team"]["id"], "team-b");
        account.poll();
        assert_eq!(count(&http, "/user/info"), 2);
        assert_eq!(count(&http, "/health/liveliness"), 2);
    }

    #[test]
    fn should_let_the_gateway_decide_a_team_while_the_list_is_unknown() {
        let (rig, http, _account, daemon) = daemon_on(idp_settings(Some("team-a")));
        http.fail("/user/info", "connection refused");
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
        assert_eq!(count(&http, "/user/info"), 2);
        let status = account.status();
        assert_eq!(status["gateway"]["url"], "https://uat.example.com");
        assert_eq!(status["gateway"]["reachable"], true);
        assert_eq!(status["user"]["email"], "dev@example.com");
    }

    #[test]
    fn should_answer_recheck_with_the_account_status_and_forward_the_rest() {
        let (rig, http, account, daemon) = daemon_on(idp_settings(Some("team-a")));
        daemon.handle(Request::SignIn, rig.peer());
        assert_eq!(account.status()["user"]["email"], "dev@example.com");
        assert_eq!(count(&http, "/user/info"), 1);
        let reply = daemon.handle(Request::Recheck, rig.peer());
        let Reply::Account(status) = reply else {
            panic!("expected the account status, got {reply:?}");
        };
        assert_eq!(count(&http, "/user/info"), 2);
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
