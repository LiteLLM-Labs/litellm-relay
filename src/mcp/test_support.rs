use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use serde_json::{json, Map, Value};
use tokio::sync::Semaphore;

use super::{
    service::{Answer, Asker, McpDependencies, McpService, Question, Switcher},
    upstream::{GatewayTarget, Upstream, UpstreamError, UpstreamFuture, UpstreamTool},
};
use crate::{
    broker::{
        test_support::{Rig, GATEWAY},
        Refusal, Reply, Switched,
    },
    config::RelaySettings,
};

pub(crate) const UAT_GATEWAY: &str = "https://uat.example.com";

pub(crate) type RecordedCall = (GatewayTarget, String, Option<Map<String, Value>>);

#[derive(Default)]
struct UpstreamState {
    tools: Vec<UpstreamTool>,
    fail_lists: bool,
    hold_calls: bool,
    lists: Vec<GatewayTarget>,
    calls: Vec<RecordedCall>,
    running: usize,
    peak: usize,
    finished: usize,
}

#[derive(Clone)]
pub(crate) struct FakeUpstream {
    state: Arc<Mutex<UpstreamState>>,
    release: Arc<Semaphore>,
}

impl FakeUpstream {
    pub(crate) fn serving(tools: Vec<UpstreamTool>) -> Self {
        Self {
            state: Arc::new(Mutex::new(UpstreamState {
                tools,
                ..UpstreamState::default()
            })),
            release: Arc::new(Semaphore::new(0)),
        }
    }

    pub(crate) fn set_tools(&self, tools: Vec<UpstreamTool>) {
        self.state.lock().unwrap().tools = tools;
    }

    pub(crate) fn fail_lists(&self, fail: bool) {
        self.state.lock().unwrap().fail_lists = fail;
    }

    pub(crate) fn hold_calls(&self) {
        self.state.lock().unwrap().hold_calls = true;
    }

    pub(crate) fn release_calls(&self, count: usize) {
        self.release.add_permits(count);
    }

    pub(crate) fn lists(&self) -> Vec<GatewayTarget> {
        self.state.lock().unwrap().lists.clone()
    }

    pub(crate) fn calls(&self) -> Vec<RecordedCall> {
        self.state.lock().unwrap().calls.clone()
    }

    pub(crate) fn running(&self) -> usize {
        self.state.lock().unwrap().running
    }

    pub(crate) fn peak(&self) -> usize {
        self.state.lock().unwrap().peak
    }

    pub(crate) fn finished(&self) -> usize {
        self.state.lock().unwrap().finished
    }
}

impl Upstream for FakeUpstream {
    fn list_tools<'a>(
        &'a self,
        target: &'a GatewayTarget,
    ) -> UpstreamFuture<'a, Vec<UpstreamTool>> {
        Box::pin(async move {
            let mut state = self.state.lock().unwrap();
            state.lists.push(target.clone());
            match state.fail_lists {
                true => Err(UpstreamError("the Gateway is down".to_string())),
                false => Ok(state.tools.clone()),
            }
        })
    }

    fn call_tool<'a>(
        &'a self,
        target: &'a GatewayTarget,
        name: &'a str,
        arguments: Option<Map<String, Value>>,
    ) -> UpstreamFuture<'a, Value> {
        Box::pin(async move {
            let hold = {
                let mut state = self.state.lock().unwrap();
                state
                    .calls
                    .push((target.clone(), name.to_string(), arguments.clone()));
                state.running += 1;
                state.peak = state.peak.max(state.running);
                state.hold_calls
            };
            if hold {
                self.release.acquire().await.expect("release gate").forget();
            }
            let mut state = self.state.lock().unwrap();
            state.running -= 1;
            state.finished += 1;
            Ok(json!({
                "content": [{"type": "text", "text": format!("ran {name}")}],
                "structuredContent": {"arguments": arguments},
                "isError": false,
            }))
        })
    }
}

#[derive(Default)]
pub(crate) struct FakeAsker {
    answers: VecDeque<Answer>,
    before_answering: Option<Box<dyn Fn() + Send>>,
    pub(crate) questions: Vec<Question>,
}

impl FakeAsker {
    pub(crate) fn answering(answers: &[Answer]) -> Self {
        Self {
            answers: answers.iter().copied().collect(),
            before_answering: None,
            questions: Vec::new(),
        }
    }

    pub(crate) fn doing_before_answering(
        answers: &[Answer],
        effect: impl Fn() + Send + 'static,
    ) -> Self {
        Self {
            before_answering: Some(Box::new(effect)),
            ..Self::answering(answers)
        }
    }
}

impl Asker for FakeAsker {
    fn ask<'a>(
        &'a mut self,
        question: Question,
    ) -> Pin<Box<dyn Future<Output = Answer> + Send + 'a>> {
        Box::pin(async move {
            self.questions.push(question);
            if let Some(effect) = self.before_answering.as_ref() {
                effect();
            }
            self.answers.pop_front().unwrap_or(Answer::Unsupported)
        })
    }
}

struct SwitcherState {
    team: Option<String>,
    environment: String,
    switches: Vec<(String, String)>,
    refusal: Option<Refusal>,
}

#[derive(Clone)]
pub(crate) struct FakeSwitcher {
    state: Arc<Mutex<SwitcherState>>,
}

impl Default for FakeSwitcher {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(SwitcherState {
                team: Some("team-a".to_string()),
                environment: "dev".to_string(),
                switches: Vec::new(),
                refusal: None,
            })),
        }
    }
}

impl FakeSwitcher {
    pub(crate) fn refusing(&self, refusal: Refusal) {
        self.state.lock().unwrap().refusal = Some(refusal);
    }

    pub(crate) fn switches(&self) -> Vec<(String, String)> {
        self.state.lock().unwrap().switches.clone()
    }

    fn switch(&self, kind: &str, value: &str) -> Reply {
        let mut state = self.state.lock().unwrap();
        if let Some(refusal) = state.refusal.clone() {
            return Reply::Refused(refusal);
        }
        state.switches.push((kind.to_string(), value.to_string()));
        match kind {
            "team" => state.team = Some(value.to_string()),
            _ => state.environment = value.to_string(),
        }
        Reply::Switched(Switched {
            team: state.team.clone(),
            environment: Some(state.environment.clone()),
            gateway_url: gateway_of(&state.environment).to_string(),
            key_expires_at: Some("2027-01-15T08:00:00Z".to_string()),
            source: Some("minted_key"),
        })
    }
}

fn gateway_of(environment: &str) -> &'static str {
    match environment {
        "uat" => UAT_GATEWAY,
        _ => GATEWAY,
    }
}

impl Switcher for FakeSwitcher {
    fn switch_team(&self, team: &str) -> Reply {
        self.switch("team", team)
    }

    fn switch_environment(&self, name: &str) -> Reply {
        self.switch("environment", name)
    }

    fn selection(&self) -> Value {
        let state = self.state.lock().unwrap();
        json!({
            "team": state.team,
            "environment": state.environment,
            "gateway_url": gateway_of(&state.environment),
            "teams": [{"id": "team-a", "alias": "Team A"}, {"id": "team-b", "alias": null}],
            "teams_error": null,
            "environments": [{"name": "dev", "url": GATEWAY}, {"name": "uat", "url": UAT_GATEWAY}],
        })
    }
}

pub(crate) fn service_on(
    settings: RelaySettings,
    upstream: &FakeUpstream,
) -> (Rig, Arc<McpService>) {
    service_with(settings, upstream, &FakeSwitcher::default())
}

pub(crate) fn service_with(
    settings: RelaySettings,
    upstream: &FakeUpstream,
    switcher: &FakeSwitcher,
) -> (Rig, Arc<McpService>) {
    let section = settings.mcp.clone();
    let rig = Rig::new(settings);
    let deps = McpDependencies {
        upstream: Arc::new(upstream.clone()),
        settings: Box::new(rig.settings.clone()),
        clock: Box::new(rig.clock.clone()),
        switcher: Arc::new(switcher.clone()),
    };
    let service = Arc::new(McpService::new(Arc::clone(&rig.broker), &section, deps));
    (rig, service)
}
