use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use serde_json::{json, Map, Value};
use tokio::sync::Semaphore;

use super::{
    service::{Answer, Asker, McpDependencies, McpService, Question},
    upstream::{GatewayTarget, Upstream, UpstreamError, UpstreamFuture, UpstreamTool},
};
use crate::{broker::test_support::Rig, config::RelaySettings};

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
    pub(crate) questions: Vec<Question>,
}

impl FakeAsker {
    pub(crate) fn answering(answers: &[Answer]) -> Self {
        Self {
            answers: answers.iter().copied().collect(),
            questions: Vec::new(),
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
            self.answers.pop_front().unwrap_or(Answer::Unsupported)
        })
    }
}

pub(crate) fn service_on(
    settings: RelaySettings,
    upstream: &FakeUpstream,
) -> (Rig, Arc<McpService>) {
    let section = settings.mcp.clone();
    let rig = Rig::new(settings);
    let deps = McpDependencies {
        upstream: Arc::new(upstream.clone()),
        settings: Box::new(rig.settings.clone()),
        clock: Box::new(rig.clock.clone()),
    };
    let service = Arc::new(McpService::new(Arc::clone(&rig.broker), &section, deps));
    (rig, service)
}
