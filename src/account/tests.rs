use std::{sync::Arc, time::Duration};

use serde_json::{json, Value};

use super::{test_support::account_on, test_support::FakeHttp, AccountService};
use crate::{
    broker::{
        rfc3339,
        test_support::{idp_settings, Rig, GATEWAY, NOW},
        Reply,
    },
    config::{EnvironmentEntry, RelaySettings},
};

const UAT: &str = "https://uat.example.com";
const USER_INFO: &str = r#"{"user_id":"user-1","user_info":{"user_id":"user-1","user_email":"dev@example.com","user_role":"internal_user"},"keys":[],"teams":[{"team_id":"team-a","team_alias":"Team A","max_budget":50.0,"spend":1.25,"budget_duration":"1mo","budget_reset_at":"2027-02-01T00:00:00+00:00","models":[]},{"team_id":"team-b","team_alias":null,"max_budget":null,"spend":0.0,"budget_duration":null,"budget_reset_at":null,"models":[]}]}"#;
const TEAM_INFO: &str = r#"{"team_id":"team-a","team_info":{"team_id":"team-a","spend":1.5,"max_budget":50.0,"budget_duration":"1mo","budget_reset_at":"2027-02-01T00:00:00+00:00","models":[]},"keys":[],"team_memberships":[]}"#;
const TEAM_B_INFO: &str = r#"{"team_id":"team-b","team_info":{"team_id":"team-b","spend":0.0,"max_budget":null,"budget_duration":null,"budget_reset_at":null,"models":[]}}"#;
const NOT_A_MEMBER: &str = r#"{"error":{"message":"User=user-1 not authorized to access this team=team-a","type":"auth_error","param":null,"code":"403"}}"#;

fn signed_in() -> (Rig, FakeHttp, Arc<AccountService>) {
    let (rig, http, account) = account_on(idp_settings(Some("team-a")));
    assert!(matches!(rig.broker.sign_in(), Reply::SignedIn { .. }));
    http.answer("/user/info", 200, USER_INFO);
    http.answer("/team/info?team_id=team-a", 200, TEAM_INFO);
    http.answer("/team/info?team_id=team-b", 200, TEAM_B_INFO);
    http.answer("/health/liveliness", 200, "\"I'm alive!\"");
    (rig, http, account)
}

fn environments_settings() -> RelaySettings {
    let mut settings = idp_settings(Some("team-a"));
    settings.environments = vec![
        EnvironmentEntry {
            name: "dev".to_string(),
            url: GATEWAY.to_string(),
            team: None,
        },
        EnvironmentEntry {
            name: "uat".to_string(),
            url: UAT.to_string(),
            team: Some("uat-team".to_string()),
        },
    ];
    settings
}

fn count(http: &FakeHttp, route: &str) -> usize {
    http.calls_to(route).len()
}

#[test]
fn should_read_teams_email_and_the_team_budget_with_the_daemon_bearer() {
    let (rig, http, account) = signed_in();
    account.poll();
    let status = account.status();
    assert_eq!(
        status["user"],
        json!({"id": "user-1", "email": "dev@example.com"})
    );
    assert_eq!(
        status["teams"],
        json!([{"id": "team-a", "alias": "Team A"}, {"id": "team-b", "alias": null}])
    );
    assert_eq!(status["teams_error"], Value::Null);
    assert_eq!(
        status["team"],
        json!({
            "id": "team-a",
            "alias": "Team A",
            "spend": 1.5,
            "max_budget": 50.0,
            "budget_reset_at": "2027-02-01T00:00:00+00:00",
        })
    );
    assert_eq!(status["budget_error"], Value::Null);
    assert_eq!(status["polled_at"], rfc3339(NOW));
    let bearer = rig
        .broker
        .session_bearer_for_daemon()
        .expect("signed in")
        .token;
    for route in ["/user/info", "/team/info?team_id=team-a"] {
        let calls = http.calls_to(route);
        assert_eq!(calls.len(), 1, "{route}");
        assert_eq!(calls[0].url, format!("{GATEWAY}{route}"));
        assert_eq!(calls[0].bearer.as_deref(), Some(bearer.as_str()), "{route}");
        assert_eq!(calls[0].timeout, Duration::from_secs(15), "{route}");
    }
    assert!(!status.to_string().contains(&bearer));
    assert_eq!(
        account.known_teams(),
        Some(vec!["team-a".to_string(), "team-b".to_string()])
    );
}

#[test]
fn should_keep_the_user_info_budget_when_team_info_is_refused() {
    let (rig, http, account) = signed_in();
    http.answer("/team/info?team_id=team-a", 403, NOT_A_MEMBER);
    account.poll();
    let status = account.status();
    assert_eq!(
        status["budget_error"],
        "User=user-1 not authorized to access this team=team-a"
    );
    assert_eq!(status["team"]["spend"], 1.25);
    assert_eq!(status["team"]["max_budget"], 50.0);
    assert_eq!(
        status["team"]["budget_reset_at"],
        "2027-02-01T00:00:00+00:00"
    );

    http.answer(
        "/team/info?team_id=team-a",
        404,
        r#"{"error":{"message":"Team not found"}}"#,
    );
    http.answer(
        "/user/info",
        200,
        r#"{"user_id":"user-1","user_info":{"user_email":null},"teams":[{"team_id":"team-a","team_alias":"Team A"}]}"#,
    );
    rig.clock.advance(300);
    account.poll();
    let status = account.status();
    assert_eq!(status["budget_error"], "Team not found");
    assert_eq!(
        status["team"],
        json!({"id": "team-a", "alias": "Team A", "spend": null, "max_budget": null, "budget_reset_at": null})
    );
    assert_eq!(status["user"]["email"], Value::Null);
}

#[test]
fn should_degrade_to_the_current_team_when_user_info_fails_or_lists_nothing() {
    let (rig, http, account) = signed_in();
    http.fail("/user/info", "connection refused");
    account.poll();
    let status = account.status();
    assert_eq!(status["teams"], json!([{"id": "team-a", "alias": null}]));
    assert_eq!(status["teams_error"], "connection refused");
    assert_eq!(status["team"]["spend"], 1.5);
    assert_eq!(account.known_teams(), None);

    http.answer("/user/info", 500, "upstream exploded");
    rig.clock.advance(300);
    account.poll();
    assert_eq!(
        account.status()["teams_error"],
        "HTTP 500: upstream exploded"
    );

    http.answer(
        "/user/info",
        200,
        r#"{"user_id":"user-1","user_info":{},"teams":[]}"#,
    );
    rig.clock.advance(300);
    account.poll();
    let status = account.status();
    assert_eq!(status["teams"], json!([{"id": "team-a", "alias": null}]));
    assert_eq!(
        status["teams_error"],
        "the Gateway lists no teams for this user"
    );
    assert_eq!(account.known_teams(), None);
}

#[test]
fn should_recheck_everything_at_once_even_when_nothing_is_due() {
    let (_rig, http, account) = signed_in();
    account.poll();
    let rechecked = account.recheck();
    assert_eq!(count(&http, "/user/info"), 2);
    assert_eq!(count(&http, "/team/info?team_id=team-a"), 2);
    assert_eq!(count(&http, "/health/liveliness"), 2);
    assert_eq!(rechecked, account.status());
    assert_eq!(rechecked["gateway"]["reachable"], true);
    assert_eq!(rechecked["team"]["spend"], 1.5);

    account.poll();
    assert_eq!(count(&http, "/user/info"), 2);
    assert_eq!(count(&http, "/health/liveliness"), 2);
}

#[test]
fn should_poll_the_account_every_300_s_and_the_gateway_every_60_s() {
    let (rig, http, account) = signed_in();
    account.poll();
    rig.clock.advance(59);
    account.poll();
    assert_eq!(count(&http, "/health/liveliness"), 1);
    assert_eq!(count(&http, "/user/info"), 1);

    rig.clock.advance(1);
    account.poll();
    assert_eq!(count(&http, "/health/liveliness"), 2);
    assert_eq!(count(&http, "/user/info"), 1);
    assert_eq!(count(&http, "/team/info?team_id=team-a"), 1);

    rig.clock.advance(239);
    account.poll();
    assert_eq!(count(&http, "/user/info"), 1);

    rig.clock.advance(1);
    account.poll();
    assert_eq!(count(&http, "/user/info"), 2);
    assert_eq!(count(&http, "/team/info?team_id=team-a"), 2);
    assert_eq!(account.status()["polled_at"], rfc3339(NOW + 300));
}

#[test]
fn should_probe_liveliness_without_a_bearer_and_flip_reachable_on_a_connection_error() {
    let (rig, http, account) = account_on(idp_settings(Some("team-a")));
    assert_eq!(
        account.status()["gateway"],
        json!({"url": GATEWAY, "reachable": false, "checked_at": null, "error": null})
    );
    http.answer("/health/liveliness", 200, "\"I'm alive!\"");
    account.poll();
    assert_eq!(
        account.status()["gateway"],
        json!({"url": GATEWAY, "reachable": true, "checked_at": rfc3339(NOW), "error": null})
    );
    let probes = http.calls_to("/health/liveliness");
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].url, format!("{GATEWAY}/health/liveliness"));
    assert_eq!(probes[0].bearer, None);
    assert_eq!(probes[0].timeout, Duration::from_secs(5));

    http.fail("/health/liveliness", "connection refused");
    rig.clock.advance(60);
    account.poll();
    assert_eq!(
        account.status()["gateway"],
        json!({"url": GATEWAY, "reachable": false, "checked_at": rfc3339(NOW + 60), "error": "connection refused"})
    );

    http.answer("/health/liveliness", 503, "not ready");
    rig.clock.advance(60);
    account.poll();
    let gateway = account.status()["gateway"].clone();
    assert_eq!(gateway["reachable"], true);
    assert_eq!(gateway["error"], "HTTP 503: not ready");
}

#[test]
fn should_clear_the_account_when_signed_out_and_still_probe_the_gateway() {
    let (rig, http, account) = signed_in();
    account.poll();
    assert_eq!(account.status()["teams"].as_array().map(Vec::len), Some(2));

    rig.broker.sign_out();
    rig.clock.advance(60);
    account.poll();
    let status = account.status();
    assert_eq!(status["user"], json!({"id": null, "email": null}));
    assert_eq!(status["teams"], Value::Null);
    assert_eq!(status["teams_error"], Value::Null);
    assert_eq!(
        status["team"],
        json!({"id": "team-a", "alias": null, "spend": null, "max_budget": null, "budget_reset_at": null})
    );
    assert_eq!(status["budget_error"], Value::Null);
    assert_eq!(status["polled_at"], Value::Null);
    assert_eq!(account.known_teams(), None);
    assert_eq!(status["gateway"]["reachable"], true);
    assert_eq!(status["gateway"]["checked_at"], rfc3339(NOW + 60));
    assert_eq!(count(&http, "/user/info"), 1);
    assert_eq!(count(&http, "/health/liveliness"), 2);

    rig.broker.sign_in();
    account.poll();
    assert_eq!(count(&http, "/user/info"), 2);
    assert_eq!(account.status()["teams"].as_array().map(Vec::len), Some(2));
}

#[test]
fn should_fetch_at_once_after_a_team_switch_and_after_an_environment_switch() {
    let (rig, http, account) = signed_in();
    rig.settings.set(environments_settings());
    account.poll();
    assert_eq!(account.status()["team"]["id"], "team-a");

    assert!(matches!(
        rig.broker.switch_team("team-b"),
        Reply::Switched(_)
    ));
    account.poll();
    assert_eq!(count(&http, "/user/info"), 2);
    assert_eq!(count(&http, "/team/info?team_id=team-b"), 1);
    assert_eq!(
        account.status()["team"],
        json!({"id": "team-b", "alias": null, "spend": 0.0, "max_budget": null, "budget_reset_at": null})
    );

    assert!(matches!(
        rig.broker.switch_environment("uat"),
        Reply::Switched(_)
    ));
    assert_eq!(account.known_teams(), None);
    account.poll();
    let users = http.calls_to("/user/info");
    assert_eq!(users.len(), 3);
    assert_eq!(users[2].url, format!("{UAT}/user/info"));
    let probes = http.calls_to("/health/liveliness");
    assert_eq!(probes.len(), 2);
    assert_eq!(probes[1].url, format!("{UAT}/health/liveliness"));
    assert_eq!(account.status()["gateway"]["url"], UAT);
    assert_eq!(account.status()["team"]["id"], "uat-team");
}

#[test]
fn should_list_the_configured_environments_and_follow_a_settings_change() {
    let settings = environments_settings();
    let (rig, _http, account) = account_on(settings.clone());
    assert_eq!(
        account.environments(),
        json!({
            "current": "dev",
            "available": [
                {"name": "dev", "url": GATEWAY},
                {"name": "uat", "url": UAT},
            ],
        })
    );

    let mut fewer = settings.clone();
    fewer.environments.truncate(1);
    rig.settings.set(fewer);
    assert_eq!(
        account.environments(),
        json!({"current": "dev", "available": [{"name": "dev", "url": GATEWAY}]})
    );

    let (_rig, _http, unmanaged) = account_on(idp_settings(None));
    assert_eq!(
        unmanaged.environments(),
        json!({"current": null, "available": []})
    );
}
