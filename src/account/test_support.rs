use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use super::{AccountDependencies, AccountHttp, AccountService, HttpAnswer};
use crate::{broker::test_support::Rig, config::RelaySettings};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Call {
    pub url: String,
    pub bearer: Option<String>,
    pub timeout: Duration,
}

#[derive(Clone, Default)]
pub(crate) struct FakeHttp {
    answers: Arc<Mutex<HashMap<String, Result<HttpAnswer, String>>>>,
    calls: Arc<Mutex<Vec<Call>>>,
}

impl FakeHttp {
    pub(crate) fn answer(&self, route: &str, status: u16, body: &str) {
        self.answers.lock().unwrap().insert(
            route.to_string(),
            Ok(HttpAnswer {
                status,
                body: body.to_string(),
            }),
        );
    }

    pub(crate) fn fail(&self, route: &str, message: &str) {
        self.answers
            .lock()
            .unwrap()
            .insert(route.to_string(), Err(message.to_string()));
    }

    pub(crate) fn calls_to(&self, route: &str) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|call| route_of(&call.url) == route)
            .cloned()
            .collect()
    }
}

fn route_of(url: &str) -> String {
    url.splitn(4, '/')
        .nth(3)
        .map(|rest| format!("/{rest}"))
        .unwrap_or_default()
}

impl AccountHttp for FakeHttp {
    fn get(
        &self,
        url: &str,
        bearer: Option<&str>,
        timeout: Duration,
    ) -> Result<HttpAnswer, String> {
        self.calls.lock().unwrap().push(Call {
            url: url.to_string(),
            bearer: bearer.map(str::to_string),
            timeout,
        });
        let route = route_of(url);
        self.answers
            .lock()
            .unwrap()
            .get(&route)
            .cloned()
            .unwrap_or_else(|| Err(format!("no answer scripted for {route}")))
    }
}

pub(crate) fn account_on(settings: RelaySettings) -> (Rig, FakeHttp, Arc<AccountService>) {
    let rig = Rig::new(settings);
    let http = FakeHttp::default();
    let deps = AccountDependencies {
        http: Arc::new(http.clone()),
        clock: Arc::new(rig.clock.clone()),
        settings: Box::new(rig.settings.clone()),
    };
    let account = Arc::new(AccountService::new(Arc::clone(&rig.broker), deps));
    (rig, http, account)
}
