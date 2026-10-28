use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use anyhow::Context;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};

use crate::{
    ai_tools::blocking::call,
    broker::{
        rfc3339, session::SessionIdentity, Broker, BrokerStatus, Clock, SettingsSource,
        SettingsVersion, Source,
    },
    config::EnvironmentEntry,
};

pub const BUDGET_INTERVAL_SECONDS: i64 = 300;
pub const REACHABILITY_INTERVAL_SECONDS: i64 = 60;
pub const WATCH_SECONDS: i64 = 600;
pub const POLL_TICK: Duration = Duration::from_secs(10);
const ACCOUNT_TIMEOUT: Duration = Duration::from_secs(15);
const REACHABILITY_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const ERROR_EXCERPT_CHARS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpAnswer {
    pub status: u16,
    pub body: String,
}

pub trait AccountHttp: Send + Sync {
    fn get(&self, url: &str, bearer: Option<&str>, timeout: Duration)
        -> Result<HttpAnswer, String>;
}

pub struct HttpAccount;

impl AccountHttp for HttpAccount {
    fn get(
        &self,
        url: &str,
        bearer: Option<&str>,
        timeout: Duration,
    ) -> Result<HttpAnswer, String> {
        let url = url.to_string();
        let bearer = bearer.map(str::to_string);
        call(move || async move {
            let client = reqwest::Client::builder()
                .timeout(timeout)
                .connect_timeout(CONNECT_TIMEOUT.min(timeout))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .context("failed to build the Gateway HTTP client")?;
            let request = client.get(&url);
            let request = match bearer {
                Some(token) => request.bearer_auth(token),
                None => request,
            };
            let response = request.send().await.context("the Gateway request failed")?;
            let status = response.status().as_u16();
            let body = response
                .text()
                .await
                .context("failed to read the Gateway answer")?;
            Ok(HttpAnswer { status, body })
        })
        .map_err(|error| format!("{error:#}"))
    }
}

pub struct AccountDependencies {
    pub http: Arc<dyn AccountHttp>,
    pub clock: Arc<dyn Clock>,
    pub settings: Box<dyn SettingsSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TeamEntry {
    id: String,
    alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Budget {
    team: String,
    spend: f64,
    max_budget: Option<f64>,
    budget_reset_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Reachability {
    url: String,
    reachable: bool,
    checked_at: i64,
    error: Option<String>,
}

struct AccountState {
    identity: Option<SessionIdentity>,
    team: Option<String>,
    key_team: Option<String>,
    teams: Option<Vec<TeamEntry>>,
    teams_error: Option<String>,
    user_email: Option<String>,
    budget: Option<Budget>,
    budget_error: Option<String>,
    gateway: Option<Reachability>,
    polled_at: Option<i64>,
    account_checked_at: Option<i64>,
    settings_version: Option<SettingsVersion>,
    environments: Vec<EnvironmentEntry>,
    watched_until: i64,
}

impl AccountState {
    fn clear_account(&mut self) {
        self.key_team = None;
        self.teams = None;
        self.teams_error = None;
        self.user_email = None;
        self.budget = None;
        self.budget_error = None;
        self.polled_at = None;
    }
}

pub struct AccountService {
    broker: Arc<Broker>,
    deps: AccountDependencies,
    state: Mutex<AccountState>,
    pass: Mutex<()>,
}

impl AccountService {
    pub fn new(broker: Arc<Broker>, deps: AccountDependencies) -> Self {
        let watched_until = deps.clock.now() + WATCH_SECONDS;
        Self {
            broker,
            deps,
            state: Mutex::new(AccountState {
                identity: None,
                team: None,
                key_team: None,
                teams: None,
                teams_error: None,
                user_email: None,
                budget: None,
                budget_error: None,
                gateway: None,
                polled_at: None,
                account_checked_at: None,
                settings_version: None,
                environments: Vec::new(),
                watched_until,
            }),
            pass: Mutex::new(()),
        }
    }

    pub fn poll(&self) {
        let _pass = self.lock_pass();
        let now = self.deps.clock.now();
        if now > self.lock_state().watched_until {
            return;
        }
        let broker = self.broker.status();
        let (account_due, gateway_due) = {
            let state = self.lock_state();
            (
                state.identity != identity_of(&broker)
                    || state.team != broker.team
                    || due(state.account_checked_at, now, BUDGET_INTERVAL_SECONDS),
                probed_gateway(&state, &broker.gateway_url)
                    .is_none_or(|probed| now - probed.checked_at >= REACHABILITY_INTERVAL_SECONDS),
            )
        };
        if account_due {
            self.poll_account(&broker, now);
        }
        if gateway_due && self.broker.configured() {
            self.probe_gateway(&broker.gateway_url, now);
        }
    }

    pub fn recheck(&self) -> Value {
        {
            let _pass = self.lock_pass();
            let now = self.deps.clock.now();
            let broker = self.broker.status();
            self.poll_account(&broker, now);
            if self.broker.configured() {
                self.probe_gateway(&broker.gateway_url, now);
            }
        }
        self.status()
    }

    pub fn known_teams(&self) -> Option<Vec<String>> {
        let identity = self.broker.session_identity()?;
        let state = self.lock_state();
        if state.identity.as_ref() != Some(&identity) || state.teams_error.is_some() {
            return None;
        }
        state
            .teams
            .as_ref()
            .map(|teams| teams.iter().map(|team| team.id.clone()).collect())
    }

    pub fn status(&self) -> Value {
        let broker = self.broker.status();
        let mut state = self.lock_state();
        state.watched_until = self.deps.clock.now() + WATCH_SECONDS;
        let session = (state.identity == identity_of(&broker)).then_some(&*state);
        let probed = probed_gateway(&state, &broker.gateway_url);
        let known_teams = session.and_then(|session| session.teams.as_ref());
        let teams = known_teams.map(|teams| {
            teams
                .iter()
                .map(|team| json!({ "id": team.id, "alias": team.alias }))
                .collect::<Vec<Value>>()
        });
        let team_id = broker
            .team
            .clone()
            .or_else(|| session.and_then(|session| session.key_team.clone()));
        let team = team_id.as_ref().map(|id| {
            let alias = known_teams
                .and_then(|teams| teams.iter().find(|team| &team.id == id))
                .and_then(|team| team.alias.clone());
            let budget = session
                .and_then(|session| session.budget.as_ref())
                .filter(|budget| &budget.team == id);
            json!({
                "id": id,
                "alias": alias,
                "spend": budget.map(|budget| budget.spend),
                "max_budget": budget.and_then(|budget| budget.max_budget),
                "budget_reset_at": budget.and_then(|budget| budget.budget_reset_at.clone()),
            })
        });
        json!({
            "user": {
                "id": broker.user_id,
                "email": session.and_then(|session| session.user_email.as_ref()),
            },
            "teams": teams,
            "teams_error": session.and_then(|session| session.teams_error.as_ref()),
            "team": team,
            "budget_error": session.and_then(|session| session.budget_error.as_ref()),
            "gateway": {
                "url": broker.gateway_url,
                "reachable": probed.map(|probed| probed.reachable),
                "checked_at": probed.map(|probed| rfc3339(probed.checked_at)),
                "error": probed.and_then(|probed| probed.error.as_ref()),
            },
            "polled_at": session.and_then(|session| session.polled_at).map(rfc3339),
        })
    }

    pub fn environments(&self) -> Value {
        let current = self.broker.status().environment;
        let available: Vec<Value> = self
            .environments_now()
            .iter()
            .map(|entry| json!({ "name": entry.name, "url": entry.url }))
            .collect();
        json!({ "current": current, "available": available })
    }

    fn environments_now(&self) -> Vec<EnvironmentEntry> {
        let version = self.deps.settings.version();
        if version.is_none() || self.lock_state().settings_version == version {
            return self.lock_state().environments.clone();
        }
        let Some(settings) = self.deps.settings.load() else {
            return self.lock_state().environments.clone();
        };
        let mut state = self.lock_state();
        state.settings_version = version;
        state.environments = settings.environments;
        state.environments.clone()
    }

    fn poll_account(&self, broker: &BrokerStatus, now: i64) {
        let bearer = match self.broker.session_bearer_for_daemon() {
            Ok(bearer) => bearer,
            Err(_) => {
                let mut state = self.lock_state();
                state.clear_account();
                state.identity = identity_of(broker);
                state.team = broker.team.clone();
                state.account_checked_at = Some(now);
                return;
            }
        };
        let base = bearer
            .identity
            .gateway_url
            .trim_end_matches('/')
            .to_string();
        let user_info_route = match bearer.identity.user_id.as_deref() {
            Some(user_id) => format!("/user/info?user_id={}", query_value(user_id)),
            None => "/user/info".to_string(),
        };
        let user = self
            .fetch(&base, &user_info_route, &bearer.token)
            .and_then(parse_user_info);
        let key_team = match (&broker.team, broker.source) {
            (None, Some(source)) if source == Source::StaticKey.name() => {
                self.key_team(&base, &bearer.token)
            }
            _ => None,
        };
        let team = broker.team.as_deref().or(key_team.as_deref());
        let previous = self.previous_budget(&bearer.identity, team);
        let (teams, teams_error) = teams_from(&user, team);
        let (budget, budget_error) = match team {
            Some(team) => self.budget_for(&base, &bearer.token, team, user.as_ref().ok(), previous),
            None => (None, None),
        };
        let mut state = self.lock_state();
        state.identity = Some(bearer.identity);
        state.team = broker.team.clone();
        state.key_team = key_team;
        state.teams = Some(teams);
        state.teams_error = teams_error;
        state.user_email = user.ok().and_then(|info| info.email);
        state.budget = budget;
        state.budget_error = budget_error;
        state.polled_at = Some(now);
        state.account_checked_at = Some(now);
    }

    fn key_team(&self, base: &str, token: &str) -> Option<String> {
        match self
            .fetch(base, "/key/info", token)
            .and_then(parse_key_info)
        {
            Ok(team) => team,
            Err(message) => {
                eprintln!("account: the key's team could not be read: {message}");
                None
            }
        }
    }

    fn previous_budget(&self, identity: &SessionIdentity, team: Option<&str>) -> Option<Budget> {
        let state = self.lock_state();
        if state.identity.as_ref() != Some(identity) {
            return None;
        }
        state
            .budget
            .clone()
            .filter(|budget| Some(budget.team.as_str()) == team)
    }

    fn budget_for(
        &self,
        base: &str,
        token: &str,
        team: &str,
        user: Option<&UserInfo>,
        previous: Option<Budget>,
    ) -> (Option<Budget>, Option<String>) {
        let route = format!("/team/info?team_id={}", query_value(team));
        let fallback = || user.and_then(|info| info.budget_of(team));
        match self.fetch(base, &route, token) {
            Err(message) => (fallback().or(previous), Some(message)),
            Ok(answer) => match parse_team_info(answer, team) {
                Ok(budget) => (Some(budget), None),
                Err(message) => (fallback(), Some(message)),
            },
        }
    }

    fn fetch(&self, base: &str, route: &str, token: &str) -> Result<HttpAnswer, String> {
        self.deps
            .http
            .get(&format!("{base}{route}"), Some(token), ACCOUNT_TIMEOUT)
    }

    fn probe_gateway(&self, url: &str, now: i64) {
        let probe = format!("{}/health/liveliness", url.trim_end_matches('/'));
        let (reachable, error) = match self.deps.http.get(&probe, None, REACHABILITY_TIMEOUT) {
            Ok(answer) if (200..300).contains(&answer.status) => (true, None),
            Ok(answer) => (
                true,
                Some(format!("HTTP {}: {}", answer.status, excerpt(&answer.body))),
            ),
            Err(message) => (false, Some(message)),
        };
        let mut state = self.lock_state();
        state.gateway = Some(Reachability {
            url: url.to_string(),
            reachable,
            checked_at: now,
            error,
        });
    }

    fn lock_state(&self) -> MutexGuard<'_, AccountState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_pass(&self) -> MutexGuard<'_, ()> {
        self.pass.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn identity_of(status: &BrokerStatus) -> Option<SessionIdentity> {
    status.signed_in.then(|| SessionIdentity {
        gateway_url: status.gateway_url.clone(),
        user_id: status.user_id.clone(),
        team: status.team.clone(),
    })
}

fn probed_gateway<'a>(state: &'a AccountState, url: &str) -> Option<&'a Reachability> {
    state.gateway.as_ref().filter(|probed| probed.url == url)
}

fn due(checked_at: Option<i64>, now: i64, interval: i64) -> bool {
    checked_at.is_none_or(|at| now - at >= interval)
}

fn query_value(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

#[derive(Deserialize)]
struct UserInfoBody {
    #[serde(default)]
    user_info: Option<UserInfoFields>,
    #[serde(default)]
    teams: Vec<UserTeam>,
}

#[derive(Deserialize)]
struct UserInfoFields {
    #[serde(default)]
    user_email: Option<String>,
}

#[derive(Deserialize)]
struct UserTeam {
    team_id: String,
    #[serde(default)]
    team_alias: Option<String>,
    #[serde(default)]
    spend: Option<f64>,
    #[serde(default)]
    max_budget: Option<f64>,
    #[serde(default)]
    budget_reset_at: Option<String>,
}

#[derive(Deserialize)]
struct KeyInfoBody {
    info: KeyInfoFields,
}

#[derive(Deserialize)]
struct KeyInfoFields {
    #[serde(default)]
    team_id: Option<String>,
}

#[derive(Deserialize)]
struct TeamInfoBody {
    team_info: TeamInfoFields,
}

#[derive(Deserialize)]
struct TeamInfoFields {
    #[serde(default)]
    spend: Option<f64>,
    #[serde(default)]
    max_budget: Option<f64>,
    #[serde(default)]
    budget_reset_at: Option<String>,
}

struct UserInfo {
    email: Option<String>,
    teams: Vec<UserTeam>,
}

impl UserInfo {
    fn budget_of(&self, team: &str) -> Option<Budget> {
        self.teams
            .iter()
            .find(|entry| entry.team_id == team)
            .filter(|entry| entry.spend.is_some() || entry.max_budget.is_some())
            .map(|entry| Budget {
                team: team.to_string(),
                spend: entry.spend.unwrap_or(0.0),
                max_budget: entry.max_budget,
                budget_reset_at: entry.budget_reset_at.clone(),
            })
    }
}

fn parse_user_info(answer: HttpAnswer) -> Result<UserInfo, String> {
    let body: UserInfoBody = parse_body(answer, "/user/info")?;
    Ok(UserInfo {
        email: body
            .user_info
            .and_then(|fields| fields.user_email)
            .filter(|email| !email.trim().is_empty()),
        teams: body.teams,
    })
}

fn parse_key_info(answer: HttpAnswer) -> Result<Option<String>, String> {
    let body: KeyInfoBody = parse_body(answer, "/key/info")?;
    Ok(body.info.team_id.filter(|team| !team.trim().is_empty()))
}

fn parse_team_info(answer: HttpAnswer, team: &str) -> Result<Budget, String> {
    let body: TeamInfoBody = parse_body(answer, "/team/info")?;
    Ok(Budget {
        team: team.to_string(),
        spend: body.team_info.spend.unwrap_or(0.0),
        max_budget: body.team_info.max_budget,
        budget_reset_at: body.team_info.budget_reset_at,
    })
}

fn parse_body<T: DeserializeOwned>(answer: HttpAnswer, route: &str) -> Result<T, String> {
    if !(200..300).contains(&answer.status) {
        return Err(gateway_message(&answer));
    }
    serde_json::from_str(&answer.body)
        .map_err(|error| format!("unreadable {route} answer: {error}"))
}

fn gateway_message(answer: &HttpAnswer) -> String {
    serde_json::from_str::<Value>(&answer.body)
        .ok()
        .and_then(|body| body["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("HTTP {}: {}", answer.status, excerpt(&answer.body)))
}

fn excerpt(body: &str) -> String {
    body.chars()
        .take(ERROR_EXCERPT_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

fn teams_from(
    user: &Result<UserInfo, String>,
    current: Option<&str>,
) -> (Vec<TeamEntry>, Option<String>) {
    let current_only = || {
        current
            .map(|id| {
                vec![TeamEntry {
                    id: id.to_string(),
                    alias: None,
                }]
            })
            .unwrap_or_default()
    };
    match user {
        Ok(info) if !info.teams.is_empty() => (
            info.teams
                .iter()
                .map(|entry| TeamEntry {
                    id: entry.team_id.clone(),
                    alias: entry
                        .team_alias
                        .clone()
                        .filter(|alias| !alias.trim().is_empty()),
                })
                .collect(),
            None,
        ),
        Ok(_) => (
            current_only(),
            Some("the Gateway lists no teams for this user".to_string()),
        ),
        Err(message) => (current_only(), Some(message.clone())),
    }
}

pub async fn poll_forever(account: Arc<AccountService>, every: Duration) {
    let mut interval = tokio::time::interval(every);
    loop {
        interval.tick().await;
        let account = Arc::clone(&account);
        if tokio::task::spawn_blocking(move || account.poll())
            .await
            .is_err()
        {
            eprintln!("account: the poll panicked; the next tick runs anyway");
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;
