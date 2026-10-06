use std::{sync::Arc, time::Duration};

use serde_json::{json, Value};

use super::*;
use crate::{
    broker::test_support::{idp_settings, static_key_settings, Rig, GATEWAY},
    config::RelaySettings,
    mcp::{
        catalog::tests::tool,
        test_support::{service_on, FakeAsker, FakeUpstream},
        upstream::UpstreamTool,
    },
};

fn tools() -> Vec<UpstreamTool> {
    vec![
        tool("github-get_issue", "Read one issue"),
        tool("github-create_issue", "Open an issue"),
        tool("jira-search", "Search issues"),
    ]
}

fn settings_with(allow: &[&str], cap: Option<usize>) -> RelaySettings {
    let mut settings = static_key_settings("sk-static");
    settings.mcp.allow = Some(allow.iter().map(|name| name.to_string()).collect());
    settings.mcp.max_concurrent_calls = cap;
    settings
}

async fn ask(
    rig: &Rig,
    service: &Arc<McpService>,
    request: Value,
    asker: &mut FakeAsker,
) -> Result<Value, McpRefusal> {
    let request: McpRequest = serde_json::from_value(request).expect("request");
    service.handle(rig.peer(), request, asker).await
}

async fn run(rig: &Rig, service: &Arc<McpService>, request: Value) -> Result<Value, McpRefusal> {
    ask(rig, service, request, &mut FakeAsker::default()).await
}

async fn activate(rig: &Rig, service: &Arc<McpService>, server: &str) {
    let mut accepting = FakeAsker::answering(&[Answer::Accept]);
    ask(
        rig,
        service,
        json!({"op": "activate_server", "server": server}),
        &mut accepting,
    )
    .await
    .expect("activated");
}

fn reason(outcome: Result<Value, McpRefusal>) -> &'static str {
    outcome.expect_err("refusal").reason()
}

async fn until(what: &str, holds: impl Fn() -> bool) {
    for _ in 0..500 {
        if holds() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting until {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_fetch_the_catalog_on_sign_in_and_drop_it_with_the_activations_on_sign_out() {
    let upstream = FakeUpstream::serving(tools());
    let (rig, service) = service_on(idp_settings(None), &upstream);
    service.check_session().await;
    assert!(upstream.lists().is_empty());
    assert_eq!(service.status()["catalog_tools"], Value::Null);

    let broker = Arc::clone(&rig.broker);
    tokio::task::spawn_blocking(move || broker.sign_in())
        .await
        .expect("sign in");
    service.check_session().await;
    assert_eq!(upstream.lists().len(), 1);
    assert_eq!(upstream.lists()[0].url, GATEWAY);
    assert_eq!(service.status()["catalog_tools"], 3);
    assert_eq!(
        service.status()["servers"]["github"],
        json!({"tools": 2, "active": false})
    );
    activate(&rig, &service, "github").await;
    assert_eq!(service.status()["servers"]["github"]["active"], true);
    assert_eq!(rig.identity.sign_ins(), 1);

    let broker = Arc::clone(&rig.broker);
    tokio::task::spawn_blocking(move || broker.sign_out())
        .await
        .expect("sign out");
    service.check_session().await;
    assert_eq!(service.status()["catalog_tools"], Value::Null);
    assert_eq!(upstream.lists().len(), 1);

    let found = run(
        &rig,
        &service,
        json!({"op": "search_tools", "query": "issue"}),
    )
    .await
    .expect("search signs in again");
    assert_eq!(rig.identity.sign_ins(), 2);
    assert_eq!(upstream.lists().len(), 2);
    assert_eq!(found, json!({"tools": [], "inactive_servers": ["github"]}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_refresh_every_300_seconds_and_keep_the_catalog_when_a_refresh_fails() {
    let upstream = FakeUpstream::serving(tools());
    let (rig, service) = service_on(static_key_settings("sk-static"), &upstream);
    service.check_session().await;
    service.check_session().await;
    assert_eq!(upstream.lists().len(), 1);
    assert_eq!(upstream.lists()[0].credential, "sk-static");
    rig.clock.advance(299);
    service.check_session().await;
    assert_eq!(upstream.lists().len(), 1);
    rig.clock.advance(1);
    service.check_session().await;
    assert_eq!(upstream.lists().len(), 2);

    upstream.fail_lists(true);
    upstream.set_tools(vec![tool("github-get_issue", "Read one issue")]);
    rig.clock.advance(300);
    service.check_session().await;
    service.check_session().await;
    assert_eq!(upstream.lists().len(), 3);
    assert_eq!(service.status()["catalog_tools"], 3);

    upstream.fail_lists(false);
    rig.clock.advance(300);
    service.check_session().await;
    assert_eq!(upstream.lists().len(), 4);
    assert_eq!(service.status()["catalog_tools"], 1);
    assert_eq!(rig.browser_opens(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_fetch_on_demand_and_refuse_the_op_when_there_is_no_catalog_to_serve() {
    let upstream = FakeUpstream::serving(tools());
    upstream.fail_lists(true);
    let (rig, service) = service_on(static_key_settings("sk-static"), &upstream);
    let search = json!({"op": "search_tools", "query": "issue"});
    assert_eq!(
        reason(run(&rig, &service, search.clone()).await),
        "catalog_unavailable"
    );
    upstream.fail_lists(false);
    assert!(run(&rig, &service, search).await.is_ok());
    assert_eq!(upstream.lists().len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_recompute_verdicts_when_the_allow_list_changes_without_a_new_fetch() {
    let upstream = FakeUpstream::serving(tools());
    let (rig, service) = service_on(static_key_settings("sk-static"), &upstream);
    activate(&rig, &service, "github").await;
    let describe = json!({"op": "describe_tool", "name": "github-create_issue"});
    let before = run(&rig, &service, describe.clone())
        .await
        .expect("described");
    assert_eq!(before["verdict"], "ask");
    assert_eq!(before["server"], "github");

    rig.settings
        .set(settings_with(&["github-create_issue"], None));
    let after = run(&rig, &service, describe.clone())
        .await
        .expect("described");
    assert_eq!(after["verdict"], "allow");
    let mut asker = FakeAsker::default();
    let call =
        json!({"op": "call_tool", "name": "github-create_issue", "arguments": {"title": "x"}});
    ask(&rig, &service, call, &mut asker)
        .await
        .expect("allowed call");
    assert!(asker.questions.is_empty());

    rig.settings.set(settings_with(&[], None));
    let tightened = run(&rig, &service, describe).await.expect("described");
    assert_eq!(tightened["verdict"], "ask");
    assert_eq!(upstream.lists().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_run_an_ask_tool_only_after_a_confirmation() {
    let upstream = FakeUpstream::serving(tools());
    let (rig, service) = service_on(static_key_settings("sk-static"), &upstream);
    activate(&rig, &service, "github").await;
    let call = json!({"op": "call_tool", "name": "github-create_issue", "arguments": {"title": "x", "labels": ["a"]}});

    for (answer, expected) in [
        (Answer::Unsupported, "confirmation_required"),
        (Answer::Decline, "declined"),
        (Answer::Cancel, "cancelled"),
    ] {
        let mut asker = FakeAsker::answering(&[answer]);
        assert_eq!(
            reason(ask(&rig, &service, call.clone(), &mut asker).await),
            expected
        );
        assert_eq!(asker.questions.len(), 1);
        assert_eq!(asker.questions[0].kind, QuestionKind::ConfirmCall);
        assert!(asker.questions[0].message.contains("github-create_issue"));
    }
    assert!(upstream.calls().is_empty());

    let mut accepting = FakeAsker::answering(&[Answer::Accept]);
    let result = ask(&rig, &service, call.clone(), &mut accepting)
        .await
        .expect("ran");
    assert_eq!(upstream.calls().len(), 1);
    let (target, name, arguments) = upstream.calls().remove(0);
    assert_eq!(
        (target.url.as_str(), target.credential.as_str()),
        (GATEWAY, "sk-static")
    );
    assert_eq!(name, "github-create_issue");
    assert_eq!(
        Value::from(arguments.expect("arguments")),
        json!({"title": "x", "labels": ["a"]})
    );
    assert_eq!(
        result["structuredContent"]["arguments"],
        json!({"title": "x", "labels": ["a"]})
    );
    assert_eq!(result["isError"], false);

    let mut claimed = call.clone();
    claimed["confirmed"] = json!(true);
    let mut silent = FakeAsker::default();
    assert_eq!(
        reason(ask(&rig, &service, claimed.clone(), &mut silent).await),
        "confirmation_required"
    );
    assert_eq!(silent.questions.len(), 1);
    let mut declining = FakeAsker::answering(&[Answer::Decline]);
    assert_eq!(
        reason(ask(&rig, &service, claimed, &mut declining).await),
        "declined"
    );
    assert_eq!(upstream.calls().len(), 1);

    let mut unasked = FakeAsker::default();
    let read = json!({"op": "call_tool", "name": "github-get_issue"});
    ask(&rig, &service, read, &mut unasked).await.expect("ran");
    assert!(unasked.questions.is_empty());
    assert_eq!(upstream.calls().len(), 2);
    assert_eq!(upstream.calls()[1].2, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_refuse_tools_on_inactive_servers_and_activate_only_on_confirmation() {
    let upstream = FakeUpstream::serving(tools());
    let (rig, service) = service_on(static_key_settings("sk-static"), &upstream);
    let call = json!({"op": "call_tool", "name": "github-get_issue"});
    let refused = run(&rig, &service, call.clone())
        .await
        .expect_err("inactive");
    assert_eq!(refused.reason(), "server_inactive");
    assert!(refused.message().contains("activate_server"));
    let describe = json!({"op": "describe_tool", "name": "github-get_issue"});
    assert_eq!(
        reason(run(&rig, &service, describe.clone()).await),
        "server_inactive"
    );
    let unknown = json!({"op": "call_tool", "name": "github-nope"});
    assert_eq!(reason(run(&rig, &service, unknown).await), "unknown_tool");
    let missing = json!({"op": "activate_server", "server": "gitlab"});
    assert_eq!(reason(run(&rig, &service, missing).await), "unknown_server");

    let activation = json!({"op": "activate_server", "server": "github"});
    let mut declining = FakeAsker::answering(&[Answer::Decline]);
    assert_eq!(
        reason(ask(&rig, &service, activation.clone(), &mut declining).await),
        "declined"
    );
    assert_eq!(declining.questions[0].kind, QuestionKind::ActivateServer);
    assert_eq!(
        reason(run(&rig, &service, activation.clone()).await),
        "confirmation_required"
    );
    assert_eq!(
        reason(run(&rig, &service, call.clone()).await),
        "server_inactive"
    );
    assert!(upstream.calls().is_empty());

    let mut accepting = FakeAsker::answering(&[Answer::Accept]);
    let activated = ask(&rig, &service, activation.clone(), &mut accepting)
        .await
        .expect("activated");
    assert_eq!(activated, json!({"server": "github", "active": true}));
    let mut already = FakeAsker::default();
    ask(&rig, &service, activation, &mut already)
        .await
        .expect("already active");
    assert!(already.questions.is_empty());

    run(&rig, &service, call).await.expect("ran");
    assert_eq!(upstream.calls().len(), 1);
    assert!(run(&rig, &service, describe).await.is_ok());
    let found = run(
        &rig,
        &service,
        json!({"op": "search_tools", "query": "issues issue"}),
    )
    .await
    .expect("found");
    assert_eq!(
        found,
        json!({"tools": ["github-create_issue", "github-get_issue"], "inactive_servers": ["jira"]})
    );
    let jira = json!({"op": "call_tool", "name": "jira-search"});
    assert_eq!(reason(run(&rig, &service, jira).await), "server_inactive");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn should_give_a_refused_caller_no_catalog_and_no_call() {
    let upstream = FakeUpstream::serving(tools());
    let (rig, service) = service_on(static_key_settings("sk-static"), &upstream);
    rig.callers.refuse(&["/usr/bin/curl"]);
    for request in [
        json!({"op": "search_tools", "query": "issue"}),
        json!({"op": "describe_tool", "name": "github-get_issue"}),
        json!({"op": "call_tool", "name": "github-get_issue"}),
        json!({"op": "activate_server", "server": "github"}),
    ] {
        assert_eq!(reason(run(&rig, &service, request).await), "caller_refused");
    }
    assert_eq!(rig.callers.checks(), 4);
    assert!(upstream.lists().is_empty());
    assert!(upstream.calls().is_empty());
}

async fn overlap_at_most(settings: RelaySettings, cap: usize) {
    let upstream = FakeUpstream::serving(tools());
    upstream.hold_calls();
    let (rig, service) = service_on(settings, &upstream);
    let rig = Arc::new(rig);
    activate(&rig, &service, "github").await;
    let callers: Vec<_> = (0..cap + 1)
        .map(|_| {
            let (rig, service) = (Arc::clone(&rig), Arc::clone(&service));
            tokio::spawn(async move {
                run(
                    &rig,
                    &service,
                    json!({"op": "call_tool", "name": "github-get_issue"}),
                )
                .await
            })
        })
        .collect();
    until("the cap is reached", || upstream.running() == cap).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(upstream.calls().len(), cap);
    assert_eq!(upstream.running(), cap);

    upstream.release_calls(1);
    until("the waiting call starts", || {
        upstream.calls().len() == cap + 1
    })
    .await;
    assert_eq!(upstream.finished(), 1);
    upstream.release_calls(cap);
    for caller in callers {
        assert!(caller.await.expect("joined").is_ok());
    }
    assert_eq!(upstream.peak(), cap);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn should_overlap_calls_up_to_the_configured_cap_and_queue_the_next_one() {
    overlap_at_most(settings_with(&[], Some(3)), 3).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn should_cap_overlapping_calls_at_sixteen_by_default_and_when_set_to_zero() {
    overlap_at_most(static_key_settings("sk-static"), 16).await;
    overlap_at_most(settings_with(&[], Some(0)), 16).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn should_apply_a_cap_lowered_in_the_config_to_the_calls_that_follow() {
    let upstream = FakeUpstream::serving(tools());
    upstream.hold_calls();
    let (rig, service) = service_on(settings_with(&[], Some(4)), &upstream);
    let rig = Arc::new(rig);
    activate(&rig, &service, "github").await;
    rig.settings.set(settings_with(&[], Some(2)));
    let callers: Vec<_> = (0..3)
        .map(|_| {
            let (rig, service) = (Arc::clone(&rig), Arc::clone(&service));
            tokio::spawn(async move {
                run(
                    &rig,
                    &service,
                    json!({"op": "call_tool", "name": "github-get_issue"}),
                )
                .await
            })
        })
        .collect();
    until("the lowered cap is reached", || upstream.running() == 2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(upstream.calls().len(), 2);
    upstream.release_calls(3);
    for caller in callers {
        assert!(caller.await.expect("joined").is_ok());
    }
    assert_eq!(upstream.peak(), 2);
}

#[tokio::test(start_paused = true)]
async fn should_answer_a_timeout_when_the_gateway_holds_a_call_past_the_ceiling() {
    let upstream = FakeUpstream::serving(tools());
    upstream.hold_calls();
    let (rig, service) = service_on(static_key_settings("sk-static"), &upstream);
    activate(&rig, &service, "github").await;
    let held = run(
        &rig,
        &service,
        json!({"op": "call_tool", "name": "github-get_issue"}),
    )
    .await
    .expect_err("timeout");
    assert_eq!(held.reason(), "upstream_timeout");
    assert!(held.message().contains("300 s"));
    upstream.release_calls(1);
    assert!(run(
        &rig,
        &service,
        json!({"op": "call_tool", "name": "github-get_issue"})
    )
    .await
    .is_ok());
}
