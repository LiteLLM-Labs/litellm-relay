use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::timeout,
};

use super::{
    catalog::{ActiveServers, Catalog},
    upstream::{GatewayTarget, Upstream},
    verdict::Verdict,
};
use crate::{
    broker::{
        caller::Peer,
        session::{SessionBearer, SessionIdentity},
        Broker, Clock, Context, Refusal, Reply, SettingsSource, SettingsVersion,
    },
    config::McpSection,
};

pub const SESSION_CHECK_INTERVAL: Duration = Duration::from_secs(5);
pub const CATALOG_REFRESH_SECONDS: i64 = 300;
pub const CALL_CEILING: Duration = Duration::from_secs(300);
pub const SHOWN_ARGUMENT_BYTES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum McpRequest {
    SearchTools {
        query: String,
    },
    DescribeTool {
        name: String,
    },
    CallTool {
        name: String,
        #[serde(default)]
        arguments: Option<Map<String, Value>>,
    },
    ActivateServer {
        server: String,
    },
    SwitchTeam {
        #[serde(default)]
        team: Option<String>,
    },
    SwitchEnvironment {
        #[serde(default)]
        environment: Option<String>,
    },
}

impl McpRequest {
    fn label(&self) -> &'static str {
        match self {
            McpRequest::SearchTools { .. } => "mcp search_tools",
            McpRequest::DescribeTool { .. } => "mcp describe_tool",
            McpRequest::CallTool { .. } => "mcp call_tool",
            McpRequest::ActivateServer { .. } => "mcp activate_server",
            McpRequest::SwitchTeam { .. } => "mcp switch_team",
            McpRequest::SwitchEnvironment { .. } => "mcp switch_environment",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    ConfirmCall,
    ActivateServer,
    SwitchTeam,
    SwitchEnvironment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Question {
    pub kind: QuestionKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Accept,
    Decline,
    Cancel,
    Unsupported,
}

pub trait Asker: Send {
    fn ask<'a>(
        &'a mut self,
        question: Question,
    ) -> Pin<Box<dyn Future<Output = Answer> + Send + 'a>>;
}

pub trait Switcher: Send + Sync {
    fn switch_team(&self, team: &str) -> Reply;
    fn switch_environment(&self, name: &str) -> Reply;
    fn selection(&self) -> Value;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpRefusal {
    Broker(Refusal),
    BadRequest(String),
    RequestTooLarge(String),
    CatalogUnavailable(String),
    UnknownTool(String),
    UnknownServer(String),
    ServerInactive(String),
    ConfirmationRequired(String),
    Declined(String),
    Cancelled(String),
    UpstreamError(String),
    UpstreamTimeout(String),
}

impl McpRefusal {
    pub fn reason(&self) -> &'static str {
        match self {
            McpRefusal::Broker(refusal) => refusal.reason(),
            McpRefusal::BadRequest(_) => "bad_request",
            McpRefusal::RequestTooLarge(_) => "request_too_large",
            McpRefusal::CatalogUnavailable(_) => "catalog_unavailable",
            McpRefusal::UnknownTool(_) => "unknown_tool",
            McpRefusal::UnknownServer(_) => "unknown_server",
            McpRefusal::ServerInactive(_) => "server_inactive",
            McpRefusal::ConfirmationRequired(_) => "confirmation_required",
            McpRefusal::Declined(_) => "declined",
            McpRefusal::Cancelled(_) => "cancelled",
            McpRefusal::UpstreamError(_) => "upstream_error",
            McpRefusal::UpstreamTimeout(_) => "upstream_timeout",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            McpRefusal::Broker(refusal) => refusal.message(),
            McpRefusal::BadRequest(message)
            | McpRefusal::RequestTooLarge(message)
            | McpRefusal::CatalogUnavailable(message)
            | McpRefusal::UnknownTool(message)
            | McpRefusal::UnknownServer(message)
            | McpRefusal::ServerInactive(message)
            | McpRefusal::ConfirmationRequired(message)
            | McpRefusal::Declined(message)
            | McpRefusal::Cancelled(message)
            | McpRefusal::UpstreamError(message)
            | McpRefusal::UpstreamTimeout(message) => message,
        }
    }
}

async fn confirm(question: Question, asker: &mut dyn Asker) -> Result<(), McpRefusal> {
    match asker.ask(question).await {
        Answer::Accept => Ok(()),
        Answer::Decline => Err(McpRefusal::Declined(
            "the user declined; nothing was run".to_string(),
        )),
        Answer::Cancel => Err(McpRefusal::Cancelled(
            "the confirmation was cancelled; nothing was run".to_string(),
        )),
        Answer::Unsupported => Err(McpRefusal::ConfirmationRequired(
            "this needs the user's confirmation in the client's own dialog and the client cannot show one; nothing was run"
                .to_string(),
        )),
    }
}

struct SlotLedger {
    cap: usize,
    owed: usize,
}

struct CallSlots {
    semaphore: Arc<Semaphore>,
    ledger: Mutex<SlotLedger>,
}

struct CallSlot {
    permit: Option<OwnedSemaphorePermit>,
    slots: Arc<CallSlots>,
}

impl CallSlots {
    fn new(cap: usize) -> Arc<Self> {
        let bounded = cap.min(Semaphore::MAX_PERMITS);
        Arc::new(Self {
            semaphore: Arc::new(Semaphore::new(bounded)),
            ledger: Mutex::new(SlotLedger {
                cap: bounded,
                owed: 0,
            }),
        })
    }

    fn lock_ledger(&self) -> MutexGuard<'_, SlotLedger> {
        self.ledger.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn resize(&self, cap: usize) {
        let bounded = cap.min(Semaphore::MAX_PERMITS);
        let mut ledger = self.lock_ledger();
        if bounded > ledger.cap {
            let added = bounded - ledger.cap;
            let repaid = added.min(ledger.owed);
            ledger.owed -= repaid;
            self.semaphore.add_permits(added - repaid);
        }
        if bounded < ledger.cap {
            let removed = ledger.cap - bounded;
            ledger.owed += removed - self.semaphore.forget_permits(removed);
        }
        ledger.cap = bounded;
    }

    async fn acquire(self: &Arc<Self>) -> Option<CallSlot> {
        let permit = Arc::clone(&self.semaphore).acquire_owned().await.ok()?;
        Some(CallSlot {
            permit: Some(permit),
            slots: Arc::clone(self),
        })
    }
}

impl Drop for CallSlot {
    fn drop(&mut self) {
        let Some(permit) = self.permit.take() else {
            return;
        };
        let mut ledger = self.slots.lock_ledger();
        if ledger.owed == 0 {
            return;
        }
        ledger.owed -= 1;
        permit.forget();
    }
}

struct Session {
    identity: SessionIdentity,
    catalog: Option<Arc<Catalog>>,
    active: ActiveServers,
    fetch_attempted_at: Option<i64>,
}

struct State {
    settings_version: Option<SettingsVersion>,
    section: McpSection,
    session: Option<Session>,
}

pub struct McpDependencies {
    pub upstream: Arc<dyn Upstream>,
    pub settings: Box<dyn SettingsSource>,
    pub clock: Box<dyn Clock>,
    pub switcher: Arc<dyn Switcher>,
}

pub struct McpService {
    broker: Arc<Broker>,
    deps: McpDependencies,
    state: Mutex<State>,
    slots: Arc<CallSlots>,
}

impl McpService {
    pub fn new(broker: Arc<Broker>, section: &McpSection, deps: McpDependencies) -> Self {
        Self {
            broker,
            slots: CallSlots::new(section.concurrent_call_cap()),
            state: Mutex::new(State {
                settings_version: deps.settings.version(),
                section: section.clone(),
                session: None,
            }),
            deps,
        }
    }

    pub async fn handle(
        self: &Arc<Self>,
        peer: Peer,
        request: McpRequest,
        asker: &mut dyn Asker,
    ) -> Result<Value, McpRefusal> {
        let service = Arc::clone(self);
        let label = request.label();
        let bearer = blocking(move || service.admit(peer, label)).await??;
        match request {
            McpRequest::SearchTools { query } => {
                let catalog = self.catalog_for(&bearer).await?;
                Ok(self.search(&catalog, &bearer, &query))
            }
            McpRequest::DescribeTool { name } => {
                let catalog = self.catalog_for(&bearer).await?;
                self.describe(&catalog, &bearer, &name)
            }
            McpRequest::CallTool { name, arguments } => {
                let catalog = self.catalog_for(&bearer).await?;
                self.call(&catalog, &bearer, &name, arguments, asker).await
            }
            McpRequest::ActivateServer { server } => {
                let catalog = self.catalog_for(&bearer).await?;
                self.activate(&catalog, &bearer, &server, asker).await
            }
            McpRequest::SwitchTeam { team } => self.switch_team(&bearer, team, asker).await,
            McpRequest::SwitchEnvironment { environment } => {
                self.switch_environment(environment, asker).await
            }
        }
    }

    pub async fn check_session(self: &Arc<Self>) {
        let service = Arc::clone(self);
        let due = tokio::task::spawn_blocking(move || service.bearer_when_a_fetch_is_due())
            .await
            .ok()
            .flatten();
        let Some(bearer) = due else {
            return;
        };
        if let Err(refusal) = self.fetch_catalog(&bearer).await {
            eprintln!(
                "mcp: the tool catalog was not refreshed, keeping the current one: {}",
                refusal.message()
            );
        }
    }

    pub fn status(&self) -> Value {
        self.adopt(self.broker.session_identity().as_ref());
        let state = self.lock_state();
        let Some(session) = state.session.as_ref() else {
            return json!({ "catalog_tools": Value::Null, "servers": {} });
        };
        let Some(catalog) = session.catalog.as_ref() else {
            return json!({ "catalog_tools": Value::Null, "servers": {} });
        };
        let servers: Map<String, Value> = catalog
            .tool_counts_by_server()
            .into_iter()
            .map(|(server, tools)| {
                let active = session.active.is_active(&server);
                (server, json!({ "tools": tools, "active": active }))
            })
            .collect();
        json!({ "catalog_tools": catalog.len(), "servers": servers })
    }

    fn admit(&self, peer: Peer, label: &str) -> Result<SessionBearer, McpRefusal> {
        let bearer = self
            .broker
            .session_bearer_for_peer(peer, label, Context::Interactive)
            .map_err(McpRefusal::Broker)?;
        self.reload_settings();
        self.adopt(Some(&bearer.identity));
        Ok(bearer)
    }

    fn bearer_when_a_fetch_is_due(&self) -> Option<SessionBearer> {
        self.reload_settings();
        self.adopt(self.broker.session_identity().as_ref());
        let now = self.deps.clock.now();
        let due = self.lock_state().session.as_ref().is_some_and(|session| {
            session
                .fetch_attempted_at
                .is_none_or(|attempted_at| now - attempted_at >= CATALOG_REFRESH_SECONDS)
        });
        if !due {
            return None;
        }
        match self.broker.session_bearer_for_daemon() {
            Ok(bearer) => Some(bearer),
            Err(refusal) => {
                self.note_fetch_attempt(None);
                eprintln!(
                    "mcp: no credential for the tool catalog refresh: {}",
                    refusal.message()
                );
                None
            }
        }
    }

    fn reload_settings(&self) {
        let Some(version) = self.deps.settings.version() else {
            return;
        };
        if self.lock_state().settings_version == Some(version) {
            return;
        }
        let Some(settings) = self.deps.settings.load() else {
            return;
        };
        let mut state = self.lock_state();
        state.settings_version = Some(version);
        if state.section == settings.mcp {
            return;
        }
        self.slots.resize(settings.mcp.concurrent_call_cap());
        if let Some(session) = state.session.as_mut() {
            session.catalog = session.catalog.take().map(|catalog| {
                Arc::new(Catalog::clone(&catalog).with_settings(
                    settings.mcp.tool_prefix_separator(),
                    settings.mcp.allowed_tools(),
                ))
            });
        }
        state.section = settings.mcp;
    }

    fn adopt(&self, identity: Option<&SessionIdentity>) {
        let mut state = self.lock_state();
        if state.session.as_ref().map(|session| &session.identity) == identity {
            return;
        }
        state.session = identity.cloned().map(|identity| Session {
            identity,
            catalog: None,
            active: ActiveServers::default(),
            fetch_attempted_at: None,
        });
    }

    fn note_fetch_attempt(&self, identity: Option<&SessionIdentity>) {
        let now = self.deps.clock.now();
        let mut state = self.lock_state();
        let attempted = state
            .session
            .as_mut()
            .filter(|session| identity.is_none_or(|identity| &session.identity == identity));
        if let Some(session) = attempted {
            session.fetch_attempted_at = Some(now);
        }
    }

    async fn catalog_for(&self, bearer: &SessionBearer) -> Result<Arc<Catalog>, McpRefusal> {
        let held = self
            .lock_state()
            .session
            .as_ref()
            .and_then(|session| session.catalog.clone());
        match held {
            Some(catalog) => Ok(catalog),
            None => self.fetch_catalog(bearer).await,
        }
    }

    async fn fetch_catalog(&self, bearer: &SessionBearer) -> Result<Arc<Catalog>, McpRefusal> {
        self.note_fetch_attempt(Some(&bearer.identity));
        let target = gateway_target(bearer);
        let tools = match timeout(CALL_CEILING, self.deps.upstream.list_tools(&target)).await {
            Ok(Ok(tools)) => tools,
            Ok(Err(error)) => return Err(McpRefusal::CatalogUnavailable(error.0)),
            Err(_) => {
                return Err(McpRefusal::CatalogUnavailable(format!(
                    "the Gateway did not list its MCP tools within {} s",
                    CALL_CEILING.as_secs()
                )))
            }
        };
        let mut state = self.lock_state();
        let catalog = Arc::new(Catalog::build(
            tools,
            state.section.tool_prefix_separator(),
            state.section.allowed_tools(),
        ));
        let session = state
            .session
            .as_mut()
            .filter(|session| session.identity == bearer.identity);
        if let Some(session) = session {
            session.catalog = Some(Arc::clone(&catalog));
        }
        Ok(catalog)
    }

    fn active_servers(&self, bearer: &SessionBearer) -> ActiveServers {
        self.lock_state()
            .session
            .as_ref()
            .filter(|session| session.identity == bearer.identity)
            .map(|session| session.active.clone())
            .unwrap_or_default()
    }

    fn require_active(&self, bearer: &SessionBearer, server: &str) -> Result<(), McpRefusal> {
        if self.active_servers(bearer).is_active(server) {
            return Ok(());
        }
        Err(McpRefusal::ServerInactive(format!(
            "the MCP server {server} is not active; call activate_server with server set to {server}, then try again"
        )))
    }

    fn search(&self, catalog: &Catalog, bearer: &SessionBearer, query: &str) -> Value {
        let found = catalog.search(query, &self.active_servers(bearer));
        json!({ "tools": found.tools, "inactive_servers": found.inactive_servers })
    }

    fn describe(
        &self,
        catalog: &Catalog,
        bearer: &SessionBearer,
        name: &str,
    ) -> Result<Value, McpRefusal> {
        let entry = catalog.get(name).ok_or_else(|| unknown_tool(name))?;
        self.require_active(bearer, &entry.server)?;
        Ok(entry.describe())
    }

    async fn call(
        self: &Arc<Self>,
        catalog: &Catalog,
        bearer: &SessionBearer,
        name: &str,
        arguments: Option<Map<String, Value>>,
        asker: &mut dyn Asker,
    ) -> Result<Value, McpRefusal> {
        let entry = catalog.get(name).ok_or_else(|| unknown_tool(name))?;
        self.require_active(bearer, &entry.server)?;
        if entry.verdict == Verdict::Ask {
            let question = Question {
                kind: QuestionKind::ConfirmCall,
                message: format!(
                    "Run {} on the MCP server {} with {}? Relay could not show that it only reads.",
                    quoted(name),
                    quoted(&entry.server),
                    argument_preview(arguments.as_ref())
                ),
            };
            confirm(question, asker).await?;
        }
        let _slot = self.slots.acquire().await.ok_or_else(|| {
            McpRefusal::UpstreamError("the daemon is no longer running tool calls".to_string())
        })?;
        self.require_same_session(bearer).await?;
        let target = gateway_target(bearer);
        match timeout(
            CALL_CEILING,
            self.deps.upstream.call_tool(&target, name, arguments),
        )
        .await
        {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(McpRefusal::UpstreamError(error.0)),
            Err(_) => Err(McpRefusal::UpstreamTimeout(format!(
                "{name} did not answer within {} s",
                CALL_CEILING.as_secs()
            ))),
        }
    }

    async fn activate(
        self: &Arc<Self>,
        catalog: &Catalog,
        bearer: &SessionBearer,
        server: &str,
        asker: &mut dyn Asker,
    ) -> Result<Value, McpRefusal> {
        let Some(tools) = catalog.tool_counts_by_server().get(server).copied() else {
            return Err(McpRefusal::UnknownServer(format!(
                "no MCP server is named {server}; search_tools names the servers that have matching tools"
            )));
        };
        let activated = json!({ "server": server, "active": true });
        if self.active_servers(bearer).is_active(server) {
            return Ok(activated);
        }
        let question = Question {
            kind: QuestionKind::ActivateServer,
            message: format!(
                "Activate the MCP server {}? Its {tools} tools become available to search and call.",
                quoted(server)
            ),
        };
        confirm(question, asker).await?;
        self.require_same_session(bearer).await?;
        let mut state = self.lock_state();
        let session = state
            .session
            .as_mut()
            .filter(|session| session.identity == bearer.identity)
            .ok_or_else(|| {
                McpRefusal::CatalogUnavailable(
                    "the signed-in session changed while waiting; try again".to_string(),
                )
            })?;
        session.active.activate(server);
        Ok(activated)
    }

    async fn require_same_session(
        self: &Arc<Self>,
        bearer: &SessionBearer,
    ) -> Result<(), McpRefusal> {
        let service = Arc::clone(self);
        let identity = bearer.identity.clone();
        let same = tokio::task::spawn_blocking(move || {
            service.adopt(service.broker.session_identity().as_ref());
            service
                .lock_state()
                .session
                .as_ref()
                .is_some_and(|session| session.identity == identity)
        })
        .await
        .unwrap_or(false);
        if same {
            return Ok(());
        }
        Err(McpRefusal::CatalogUnavailable(
            "the signed-in session changed while waiting; try again".to_string(),
        ))
    }

    async fn switch_team(
        &self,
        bearer: &SessionBearer,
        team: Option<String>,
        asker: &mut dyn Asker,
    ) -> Result<Value, McpRefusal> {
        let Some(team) = team else {
            return self.selection().await;
        };
        let question = Question {
            kind: QuestionKind::SwitchTeam,
            message: format!("Switch the team to {}?", quoted(&team)),
        };
        confirm(question, asker).await?;
        let switcher = Arc::clone(&self.deps.switcher);
        let switched = switched(blocking(move || switcher.switch_team(&team)).await?)?;
        self.forget_fetch_time(&bearer.identity);
        self.selection_after(switched).await
    }

    async fn switch_environment(
        &self,
        environment: Option<String>,
        asker: &mut dyn Asker,
    ) -> Result<Value, McpRefusal> {
        let Some(name) = environment else {
            return self.selection().await;
        };
        let question = Question {
            kind: QuestionKind::SwitchEnvironment,
            message: match environment_url(&self.selection().await?, &name) {
                Some(url) => format!("Switch the environment to {} ({url})?", quoted(&name)),
                None => format!("Switch the environment to {}?", quoted(&name)),
            },
        };
        confirm(question, asker).await?;
        let switcher = Arc::clone(&self.deps.switcher);
        let switched = switched(blocking(move || switcher.switch_environment(&name)).await?)?;
        self.selection_after(switched).await
    }

    async fn selection(&self) -> Result<Value, McpRefusal> {
        let switcher = Arc::clone(&self.deps.switcher);
        blocking(move || switcher.selection()).await
    }

    async fn selection_after(&self, switched: Value) -> Result<Value, McpRefusal> {
        let selection = self.selection().await?;
        Ok(Value::Object(
            entries(switched)
                .into_iter()
                .chain(entries(selection))
                .collect(),
        ))
    }

    fn forget_fetch_time(&self, identity: &SessionIdentity) {
        let mut state = self.lock_state();
        let session = state
            .session
            .as_mut()
            .filter(|session| &session.identity == identity);
        if let Some(session) = session {
            session.fetch_attempted_at = None;
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, McpRefusal> {
    tokio::task::spawn_blocking(work).await.map_err(|_| {
        McpRefusal::Broker(Refusal::GatewayError(
            "the broker could not answer this request".to_string(),
        ))
    })
}

fn switched(reply: Reply) -> Result<Value, McpRefusal> {
    match reply {
        Reply::Switched(switched) => serde_json::to_value(&switched).map_err(|error| {
            McpRefusal::Broker(Refusal::GatewayError(format!(
                "the switch outcome could not be rendered: {error}"
            )))
        }),
        Reply::Refused(refusal) => Err(McpRefusal::Broker(refusal)),
        Reply::Credential(_)
        | Reply::SignedIn { .. }
        | Reply::SignedOut
        | Reply::Status(_)
        | Reply::Account(_) => Err(McpRefusal::Broker(Refusal::GatewayError(
            "the daemon answered the switch with something other than a switch outcome".to_string(),
        ))),
    }
}

fn entries(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn environment_url(selection: &Value, name: &str) -> Option<String> {
    selection["environments"]
        .as_array()?
        .iter()
        .find(|entry| entry["name"] == name)
        .and_then(|entry| entry["url"].as_str())
        .map(str::to_string)
}

fn gateway_target(bearer: &SessionBearer) -> GatewayTarget {
    GatewayTarget {
        url: bearer.identity.gateway_url.clone(),
        credential: bearer.token.clone(),
    }
}

fn unknown_tool(name: &str) -> McpRefusal {
    McpRefusal::UnknownTool(format!(
        "no tool is named {name}; call search_tools to find the exact name"
    ))
}

fn quoted(text: &str) -> String {
    format!("{text:?}")
}

fn argument_preview(arguments: Option<&Map<String, Value>>) -> String {
    let Some(arguments) = arguments.filter(|arguments| !arguments.is_empty()) else {
        return "no arguments".to_string();
    };
    let rendered = Value::Object(arguments.clone()).to_string();
    if rendered.len() <= SHOWN_ARGUMENT_BYTES {
        return format!("arguments {rendered}");
    }
    let shown = rendered
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= SHOWN_ARGUMENT_BYTES)
        .last()
        .unwrap_or(0);
    format!(
        "arguments {}... (the first {shown} of {} bytes)",
        &rendered[..shown],
        rendered.len()
    )
}

pub async fn watch_session(service: Arc<McpService>, every: Duration) {
    let mut interval = tokio::time::interval(every);
    loop {
        interval.tick().await;
        service.check_session().await;
    }
}

#[cfg(test)]
#[path = "service_tests.rs"]
pub(crate) mod tests;
