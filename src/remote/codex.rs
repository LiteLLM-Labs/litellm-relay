use std::{
    collections::{HashMap, HashSet},
    future::Future,
    io::ErrorKind,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::{
    net::UnixStream,
    process::Command,
    sync::{mpsc, oneshot, Mutex as AsyncMutex},
};
use tokio_tungstenite::tungstenite::{protocol::WebSocketConfig, Message as Frame};
use url::Url;
use uuid::Uuid;

use super::{
    protocol::{
        now_ms, public_error, redact, Agent, Approval, Decision, Detail, Failure, Message, Result,
        Session,
    },
    route,
};

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    id: Uuid,
    name: Option<String>,
    #[serde(default)]
    preview: String,
    cwd: String,
    model_provider: String,
    model: Option<String>,
    // Present only so an object-form policy cannot fail thread/read or thread/list.
    // thread/resume carries the policy beside `thread`; that is what `send` reads.
    #[allow(dead_code)]
    approval_policy: Option<Value>,
    can_accept_direct_input: Option<bool>,
    updated_at: i64,
    status: ThreadStatus,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadStatus {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    active_flags: Vec<String>,
}

impl Thread {
    fn summary(&self) -> Session {
        let active = self.status.kind == "active";
        Session {
            id: Agent::Codex.session_id(&self.id.to_string()),
            agent: Agent::Codex,
            title: redact(
                self.name
                    .as_deref()
                    .filter(|name| !name.is_empty())
                    .unwrap_or_else(|| self.preview.lines().next().unwrap_or("Untitled session")),
            )
            .chars()
            .take(140)
            .collect(),
            project: super::sessions::project(&self.cwd),
            cwd: self.cwd.clone(),
            model: self.model.clone(),
            status: if self
                .status
                .active_flags
                .iter()
                .any(|flag| flag == "waitingOnApproval")
            {
                "needs_approval"
            } else if active {
                "working"
            } else if self.status.kind == "idle" {
                "ready"
            } else {
                "saved"
            }
            .into(),
            can_send: self.can_accept_direct_input != Some(false),
            can_stop: active,
            updated_at: self.updated_at,
        }
    }
}

#[derive(Deserialize)]
struct ThreadReply {
    thread: Thread,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResumeReply {
    thread: Thread,
    // AskForApproval is either a string or the experimental granular object.
    // thread/resume sends it next to `thread`, not inside the thread object.
    #[serde(default)]
    approval_policy: Option<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    data: Vec<T>,
    next_cursor: Option<String>,
}
#[derive(Deserialize)]
struct Turn {
    id: String,
    status: String,
    #[serde(default)]
    items: Vec<Value>,
    error: Option<TurnError>,
}
#[derive(Deserialize)]
struct TurnError {
    message: String,
}

struct ApprovalRequest {
    request: Value,
    thread: String,
    turn: String,
    approval: Approval,
}
struct Live {
    thread: String,
    turn: String,
    item: String,
    text: String,
    at: i64,
    complete: bool,
}
#[derive(Default)]
struct Events {
    approvals: HashMap<Uuid, ApprovalRequest>,
    changes: HashMap<String, String>,
    live: HashMap<String, Live>,
    tracked: HashSet<String>,
    // Turns Codex has finished or interrupted. Later deltas for them are ignored
    // so a stopped turn cannot put its partial stream back on screen.
    finished: HashSet<String>,
}

impl Events {
    fn event(&mut self, value: &Value) -> Option<Value> {
        let method = value["method"].as_str()?;
        let params = &value["params"];
        let thread = params["threadId"].as_str().unwrap_or("");
        let item = params["itemId"].as_str().unwrap_or("");
        match method {
            "item/agentMessage/delta"
                if self.tracked.contains(thread)
                    && !self.finished.contains(&format!(
                        "{thread}/{}",
                        params["turnId"].as_str().unwrap_or("")
                    )) =>
            {
                let key = format!(
                    "{thread}/{}/{item}",
                    params["turnId"].as_str().unwrap_or("")
                );
                if self.live.len() >= 128 && !self.live.contains_key(&key) {
                    if let Some(oldest) = self
                        .live
                        .iter()
                        .min_by_key(|(_, live)| live.at)
                        .map(|(key, _)| key.clone())
                    {
                        self.live.remove(&oldest);
                    }
                }
                let live = self.live.entry(key).or_insert_with(|| Live {
                    thread: thread.into(),
                    turn: params["turnId"].as_str().unwrap_or("").into(),
                    item: item.into(),
                    text: String::new(),
                    at: now_ms(),
                    complete: false,
                });
                if live.text.len() < 256_000 {
                    live.text.extend(
                        params["delta"]
                            .as_str()
                            .unwrap_or("")
                            .chars()
                            .take(256_000 - live.text.len()),
                    );
                }
                live.at = now_ms();
            }
            "item/started" if params["item"]["type"] == "fileChange" => {
                if self.changes.len() >= 100 {
                    self.changes.clear();
                }
                if let Some(changes) = params["item"]["changes"].as_array() {
                    let full = changes
                        .iter()
                        .map(|change| {
                            Some(format!(
                                "{}\n{}",
                                change["path"].as_str()?,
                                change["diff"].as_str()?
                            ))
                        })
                        .collect::<Option<Vec<_>>>()
                        .map(|parts| parts.join("\n\n"));
                    if let Some(text) = full.filter(|text| !text.is_empty() && text.len() <= 32_000)
                    {
                        self.changes.insert(
                            format!("{thread}/{}", params["item"]["id"].as_str().unwrap_or("")),
                            text,
                        );
                    }
                }
            }
            "serverRequest/resolved" => self
                .approvals
                .retain(|_, approval| approval.request != params["requestId"]),
            "turn/completed" => {
                self.finish_turn(thread, params["turn"]["id"].as_str().unwrap_or(""));
            }
            "item/completed" => {
                self.changes.remove(&format!(
                    "{thread}/{}",
                    params["item"]["id"].as_str().unwrap_or("")
                ));
                if let Some(live) = self.live.get_mut(&format!(
                    "{thread}/{}/{}",
                    params["turnId"].as_str().unwrap_or(""),
                    params["item"]["id"].as_str().unwrap_or("")
                )) {
                    live.complete = true;
                }
            }
            _ => {}
        }
        let request = value.get("id")?;
        if !matches!(
            method,
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval"
        ) {
            return Some(
                json!({"id":request,"error":{"code":-32601,"message":"Review this request on the device."}}),
            );
        }
        if self
            .approvals
            .values()
            .any(|approval| approval.request == *request)
        {
            return None;
        }
        let command = method == "item/commandExecution/requestApproval";
        let changes = self.changes.get(&format!("{thread}/{item}"));
        let full = if command {
            serde_json::to_string_pretty(params).unwrap_or_default()
        } else {
            changes
                .cloned()
                .unwrap_or_else(|| "Review the complete request on the device.".into())
        };
        let can_review = if command {
            params["command"].is_string()
                || params["networkApprovalContext"].is_object()
                || params["additionalPermissions"].is_object()
        } else {
            changes.is_some() && params["grantRoot"].is_null()
        };
        let can_accept = can_review
            && full.len() <= 32_000
            && params
                .get("availableDecisions")
                .and_then(Value::as_array)
                .is_none_or(|choices| choices.iter().any(|choice| choice == "accept"));
        if thread.is_empty() || self.approvals.len() >= 128 {
            return Some(json!({"id":request,"result":{"decision":"decline"}}));
        }
        let identity = json!([
            "codex-approval",
            method,
            thread,
            params["turnId"],
            item,
            request
        ]);
        let mut bytes = [0; 16];
        bytes.copy_from_slice(&Sha256::digest(identity.to_string().as_bytes())[..16]);
        let id = uuid::Builder::from_custom_bytes(bytes).into_uuid();
        self.approvals.insert(
            id,
            ApprovalRequest {
                request: request.clone(),
                thread: thread.into(),
                turn: params["turnId"].as_str().unwrap_or("").into(),
                approval: Approval {
                    id: id.to_string(),
                    title: if command {
                        "Run this command?"
                    } else {
                        "Allow file changes?"
                    }
                    .into(),
                    text: redact(&full).chars().take(32_000).collect(),
                    can_accept,
                },
            },
        );
        None
    }

    fn finish_turn(&mut self, thread: &str, turn: &str) {
        if thread.is_empty() || turn.is_empty() {
            return;
        }
        if self.finished.len() >= 256 {
            self.finished.clear();
        }
        self.finished.insert(format!("{thread}/{turn}"));
        self.live
            .retain(|_, live| live.thread != thread || live.turn != turn);
        self.approvals
            .retain(|_, approval| approval.thread != thread || approval.turn != turn);
    }
}

struct Call {
    value: Value,
    reply: Option<oneshot::Sender<Result<Value>>>,
    written: Option<oneshot::Sender<Result<()>>>,
}
#[derive(Clone)]
struct Rpc {
    sender: mpsc::Sender<Call>,
}

impl Rpc {
    async fn request<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Call {
                value: json!({"id":Uuid::new_v4().to_string(),"method":method,"params":params}),
                reply: Some(reply),
                written: None,
            })
            .await
            .map_err(|_| disconnected())?;
        let value = tokio::time::timeout(Duration::from_secs(10), receive)
            .await
            .map_err(|_| {
                Failure::new(
                    504,
                    "Codex did not answer in time. Refresh before retrying.",
                )
            })?
            .map_err(|_| disconnected())??;
        serde_json::from_value(value)
            .map_err(|_| Failure::new(502, "This Codex response is not supported yet."))
    }
    async fn write(&self, value: Value) -> Result<()> {
        let (written, receive) = oneshot::channel();
        self.sender
            .send(Call {
                value,
                reply: None,
                written: Some(written),
            })
            .await
            .map_err(|_| disconnected())?;
        receive.await.map_err(|_| disconnected())?
    }
}

fn disconnected() -> Failure {
    Failure::new(
        503,
        "Cannot reach Codex. Keep Codex running on this device.",
    )
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    #[tokio::test]
    async fn write_waits_for_the_transport_result() {
        for succeeds in [true, false] {
            let (sender, mut receiver) = mpsc::channel(1);
            let rpc = Rpc { sender };
            let write = rpc.write(json!({"id":17,"result":{"decision":"accept"}}));
            tokio::pin!(write);
            let call = tokio::select! {
                result = &mut write => panic!("Write completed before reaching the transport: {result:?}"),
                call = receiver.recv() => call.unwrap(),
            };
            let (checked, check) = oneshot::channel();
            checked.send(()).unwrap();
            tokio::select! {
                biased;
                result = &mut write => panic!("Write completed without a transport result: {result:?}"),
                _ = check => {}
            }
            call.written
                .unwrap()
                .send(if succeeds {
                    Ok(())
                } else {
                    Err(disconnected())
                })
                .unwrap();
            let result = write.await;
            if succeeds {
                result.unwrap();
            } else {
                assert_eq!(result.unwrap_err().status, 503);
            }
        }
    }

    #[tokio::test]
    async fn write_reports_a_disconnected_transport() {
        let (sender, mut receiver) = mpsc::channel(1);
        let rpc = Rpc { sender };
        let writer = tokio::spawn(async move {
            receiver.recv().await.unwrap();
        });
        assert_eq!(rpc.write(json!({"id":17})).await.unwrap_err().status, 503);
        writer.await.unwrap();
    }
}

fn sun_path_len() -> usize {
    if cfg!(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "watchos",
        target_os = "tvos",
        target_os = "visionos"
    )) {
        104
    } else {
        108
    }
}

pub struct Codex {
    path: PathBuf,
    auto_start: bool,
    provider: String,
    gateway: Url,
    rpc: AsyncMutex<Option<Rpc>>,
    events: Arc<Mutex<Events>>,
    drafts: Mutex<HashMap<Uuid, Thread>>,
    starting: Mutex<HashMap<Uuid, Starting>>,
    mutation: AsyncMutex<()>,
}

#[derive(Clone)]
struct Starting {
    text: String,
    at: i64,
}

impl Codex {
    pub fn new(path: PathBuf, provider: String, gateway: Url) -> Self {
        Self {
            path,
            auto_start: false,
            provider,
            gateway,
            rpc: AsyncMutex::new(None),
            events: Arc::new(Mutex::new(Events::default())),
            drafts: Mutex::new(HashMap::new()),
            starting: Mutex::new(HashMap::new()),
            mutation: AsyncMutex::new(()),
        }
    }

    pub fn auto_start(mut self, enabled: bool) -> Self {
        self.auto_start = enabled;
        self
    }

    pub(super) async fn control_stream(
        &self,
        start: impl Future<Output = std::io::Result<()>>,
    ) -> std::io::Result<UnixStream> {
        match UnixStream::connect(&self.path).await {
            // A crash leaves the socket file behind, so connect fails with
            // ConnectionRefused rather than NotFound.
            Err(error)
                if self.auto_start
                    && matches!(
                        error.kind(),
                        ErrorKind::NotFound | ErrorKind::ConnectionRefused
                    ) => {}
            result => return result,
        }
        start.await?;
        loop {
            match UnixStream::connect(&self.path).await {
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::NotFound | ErrorKind::ConnectionRefused
                    ) =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                result => return result,
            }
        }
    }

    async fn start_daemon() -> std::io::Result<()> {
        let status = Self::daemon_command("codex").status().await?;
        if !status.success() {
            return Err(std::io::Error::other("Codex daemon did not start"));
        }
        Ok(())
    }

    pub(super) fn daemon_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        super::claude::process::isolate(&mut command);
        super::claude::process::strip_provider_secrets(&mut command);
        if let Some(home) = std::env::var_os("CODEX_HOME").filter(|value| !value.is_empty()) {
            command.env("CODEX_HOME", home);
        }
        if let Ok(key) = std::env::var("LITELLM_API_KEY") {
            if !key.is_empty() {
                command.env("LITELLM_API_KEY", key);
            }
        }
        command
            .args(["app-server", "daemon", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        command
    }

    async fn connection(&self) -> Result<Rpc> {
        let mut current = self.rpc.lock().await;
        if let Some(rpc) = current.as_ref().filter(|rpc| !rpc.sender.is_closed()) {
            return Ok(rpc.clone());
        }
        self.drafts.lock().expect("Codex draft lock").clear();
        self.starting.lock().expect("Codex starting lock").clear();
        if self.path.as_os_str().len() >= sun_path_len() {
            return Err(Failure::new(
                400,
                format!(
                    "The Codex socket path must be shorter than {} bytes. Shorten CODEX_HOME or pass --codex-socket.",
                    sun_path_len()
                ),
            ));
        }
        let stream = tokio::time::timeout(
            Duration::from_secs(8),
            self.control_stream(Self::start_daemon()),
        )
        .await
        .map_err(|_| disconnected())?
        .map_err(|_| disconnected())?;
        let config = WebSocketConfig::default().max_message_size(Some(32 * 1024 * 1024));
        let (socket, _) =
            tokio_tungstenite::client_async_with_config("ws://localhost/", stream, Some(config))
                .await
                .map_err(|_| disconnected())?;
        let (sender, mut receiver) = mpsc::channel::<Call>(32);
        let events = self.events.clone();
        tokio::spawn(async move {
            let (mut sink, mut source) = socket.split();
            let mut pending = HashMap::<String, oneshot::Sender<Result<Value>>>::new();
            let mut failed_write = None;
            loop {
                tokio::select! {
                    call = receiver.recv() => {
                        let Some(call) = call else { break; };
                        pending.retain(|_, reply| !reply.is_closed());
                        if let Some(reply) = call.reply { pending.insert(call.value["id"].as_str().unwrap_or("").into(), reply); }
                        let sent = tokio::time::timeout(Duration::from_secs(10), sink.send(Frame::Text(call.value.to_string().into()))).await.is_ok_and(|result| result.is_ok());
                        if !sent { failed_write = call.written; break; }
                        if let Some(written) = call.written { let _ = written.send(Ok(())); }
                    }
                    frame = source.next() => {
                        match frame {
                            Some(Ok(Frame::Text(text))) => {
                                let Ok(value) = serde_json::from_str::<Value>(&text) else { break; };
                                if value.get("method").is_some() {
                                    let response = events.lock().expect("Codex event lock").event(&value);
                                    if let Some(response) = response { if sink.send(Frame::Text(response.to_string().into())).await.is_err() { break; } }
                                } else if let Some(reply) = value["id"].as_str().and_then(|id| pending.remove(id)) {
                                    let result = if let Some(error) = value.get("error") { Err(Failure::new(502, public_error(error["message"].as_str().unwrap_or("Codex rejected the request.")))) }
                                        else { value.get("result").cloned().ok_or_else(|| Failure::new(502, "Codex returned an invalid response.")) };
                                    let _ = reply.send(result);
                                }
                            }
                            Some(Ok(Frame::Ping(bytes))) => { if sink.send(Frame::Pong(bytes)).await.is_err() { break; } }
                            Some(Ok(Frame::Pong(_))) => {}
                            _ => break,
                        }
                    }
                }
            }
            receiver.close();
            *events.lock().expect("Codex event lock") = Events::default();
            for (_, reply) in pending {
                let _ = reply.send(Err(disconnected()));
            }
            if let Some(written) = failed_write {
                let _ = written.send(Err(disconnected()));
            }
        });
        let rpc = Rpc { sender };
        let _: Value = rpc.request("initialize", json!({"clientInfo":{"name":"litellm_remote_control","title":"LiteLLM Remote","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}})).await?;
        rpc.write(json!({"method":"initialized"})).await?;
        *current = Some(rpc.clone());
        Ok(rpc)
    }

    async fn metadata(&self, rpc: &Rpc, id: Uuid) -> Result<Thread> {
        let draft = self
            .drafts
            .lock()
            .expect("Codex draft lock")
            .get(&id)
            .cloned();
        let thread = match draft {
            Some(thread) => thread,
            None => {
                rpc.request::<ThreadReply>("thread/read", json!({"threadId":id}))
                    .await?
                    .thread
            }
        };
        if thread.model_provider != self.provider || thread.id != id {
            return Err(Failure::new(404, "Session not found."));
        }
        Ok(thread)
    }

    async fn verify(&self, rpc: &Rpc, cwd: &str) -> Result<()> {
        let config: Value = rpc.request("config/read", json!({"cwd":cwd})).await?;
        let endpoint = config["config"]["model_providers"][&self.provider]["base_url"]
            .as_str()
            .and_then(|endpoint| Url::parse(endpoint).ok())
            .ok_or_else(|| Failure::new(409, "Configure Codex to use Relay’s gateway provider."))?;
        route::verify(&endpoint, &self.gateway).await.map(|_| ())
    }

    pub async fn list(&self, cursor: Option<&str>) -> Result<(Vec<Session>, Option<String>)> {
        let rpc = self.connection().await?;
        let page: Page<Thread> = rpc.request("thread/list", json!({"limit":50,"sortKey":"updated_at","modelProviders":[&self.provider],"sourceKinds":["cli","vscode","appServer"],"cursor":cursor})).await?;
        let mut sessions = page
            .data
            .iter()
            .filter(|thread| thread.model_provider == self.provider)
            .map(Thread::summary)
            .collect::<Vec<_>>();
        if cursor.is_none() {
            sessions.extend(
                self.drafts
                    .lock()
                    .expect("Codex draft lock")
                    .values()
                    .filter(|thread| !page.data.iter().any(|value| value.id == thread.id))
                    .map(Thread::summary),
            );
        }
        Ok((sessions, page.next_cursor))
    }

    pub async fn create(&self, cwd: String) -> Result<Session> {
        super::sessions::validate_directory(&cwd)?;
        let _guard = self.mutation.lock().await;
        let rpc = self.connection().await?;
        self.verify(&rpc, &cwd).await?;
        let thread = rpc.request::<ThreadReply>("thread/start", json!({"modelProvider":&self.provider,"cwd":cwd,"approvalPolicy":"on-request","sandbox":"workspace-write","serviceName":"litellm_remote"})).await?.thread;
        if thread.model_provider != self.provider {
            return Err(Failure::new(
                502,
                "Codex did not select the gateway provider.",
            ));
        }
        let summary = thread.summary();
        self.events
            .lock()
            .expect("Codex event lock")
            .tracked
            .insert(thread.id.to_string());
        self.drafts
            .lock()
            .expect("Codex draft lock")
            .insert(thread.id, thread);
        Ok(summary)
    }

    pub async fn send(&self, id: Uuid, text: String, model: Option<String>) -> Result<Value> {
        let _guard = self.mutation.lock().await;
        let rpc = self.connection().await?;
        let thread = self.metadata(&rpc, id).await?;
        self.verify(&rpc, &thread.cwd).await?;
        if thread.can_accept_direct_input == Some(false) {
            return Err(Failure::new(
                409,
                "Open the parent session to send messages.",
            ));
        }
        if thread.status.kind == "active" {
            return Err(Failure::new(
                409,
                "Wait for the current turn or stop it before sending.",
            ));
        }
        self.events
            .lock()
            .expect("Codex event lock")
            .tracked
            .insert(id.to_string());
        let mut policy = None;
        if !self
            .drafts
            .lock()
            .expect("Codex draft lock")
            .contains_key(&id)
        {
            let resumed = rpc
                .request::<ResumeReply>(
                    "thread/resume",
                    json!({"threadId":id,"modelProvider":&self.provider,"excludeTurns":true}),
                )
                .await?;
            if resumed.thread.model_provider != self.provider {
                return Err(Failure::new(409, "This session changed provider."));
            }
            policy = resumed.approval_policy;
        }
        let mut params =
            json!({"threadId":id,"input":[{"type":"text","text":text,"text_elements":[]}]});
        // A resumed thread keeps its own approval policy; only "never" is lifted to
        // "on-request", because it would otherwise deny mobile tool requests with no card.
        if policy.as_ref().and_then(Value::as_str) == Some("never") {
            params["approvalPolicy"] = json!("on-request");
        }
        if let Some(model) = &model {
            params["model"] = json!(model);
        }
        let result: Value = rpc.request("turn/start", params).await?;
        if let Some(draft) = self.drafts.lock().expect("Codex draft lock").get_mut(&id) {
            draft.status.kind = "active".into();
            draft.model = model.or_else(|| draft.model.clone());
            self.starting
                .lock()
                .expect("Codex starting lock")
                .insert(id, Starting { text, at: now_ms() });
        }
        Ok(json!({"turnId":result["turn"]["id"]}))
    }

    pub async fn read(&self, id: Uuid) -> Result<Detail> {
        self.read_with_rpc(id).await.map(|(detail, _)| detail)
    }

    async fn read_with_rpc(&self, id: Uuid) -> Result<(Detail, Rpc)> {
        let rpc = self.connection().await?;
        let thread = self.metadata(&rpc, id).await?;
        if !self
            .events
            .lock()
            .expect("Codex event lock")
            .tracked
            .contains(&id.to_string())
        {
            let resumed: ThreadReply = rpc
                .request("thread/resume", json!({"threadId":id,"excludeTurns":true}))
                .await?;
            if resumed.thread.id != id || resumed.thread.model_provider != self.provider {
                return Err(Failure::new(404, "Session not found."));
            }
        }
        let draft = self
            .drafts
            .lock()
            .expect("Codex draft lock")
            .contains_key(&id);
        let starting = self
            .starting
            .lock()
            .expect("Codex starting lock")
            .get(&id)
            .cloned();
        let turns: Page<Turn> = if draft && starting.is_none() {
            Page {
                data: Vec::new(),
                next_cursor: None,
            }
        } else {
            let loaded = rpc
                .request::<Page<Turn>>(
                    "thread/turns/list",
                    json!({"threadId":id,"limit":12,"sortDirection":"desc","itemsView":"full"}),
                )
                .await;
            match loaded {
                Ok(page) => {
                    self.drafts.lock().expect("Codex draft lock").remove(&id);
                    self.starting
                        .lock()
                        .expect("Codex starting lock")
                        .remove(&id);
                    page
                }
                Err(_)
                    if starting
                        .as_ref()
                        .is_some_and(|starting| starting.at > now_ms() - 30_000) =>
                {
                    Page {
                        data: vec![Turn {
                            id: "pending".into(),
                            status: "inProgress".into(),
                            items: vec![
                                json!({"type":"userMessage","id":"input","content":[{"type":"text","text":starting.as_ref().expect("starting turn").text}]}),
                            ],
                            error: None,
                        }],
                        next_cursor: None,
                    }
                }
                Err(error) => return Err(error),
            }
        };
        let stored = turns
            .data
            .iter()
            .rev()
            .flat_map(|turn| turn.items.iter().filter_map(|item| visible(item, &turn.id)))
            .collect::<Vec<_>>();
        let mut events = self.events.lock().expect("Codex event lock");
        if rpc.sender.is_closed() {
            return Err(disconnected());
        }
        events.tracked.insert(id.to_string());
        events.live.retain(|_, live| live.at > now_ms() - 300_000);
        let streaming_reply_id = events
            .live
            .values()
            .filter(|live| live.thread == id.to_string() && !live.complete)
            .max_by_key(|live| live.at)
            .map(|live| format!("{}/{}", live.turn, live.item));
        let live = events
            .live
            .values()
            .filter(|live| live.thread == id.to_string())
            .map(|live| Message {
                id: format!("{}/{}", live.turn, live.item),
                role: "assistant".into(),
                text: redact(&live.text),
            })
            .collect::<Vec<_>>();
        let mut messages = stored
            .iter()
            .map(|message| {
                live.iter()
                    .find(|value| value.id == message.id && value.text.len() > message.text.len())
                    .unwrap_or(message)
                    .clone()
            })
            .collect::<Vec<_>>();
        messages.extend(
            live.into_iter()
                .filter(|message| !stored.iter().any(|value| value.id == message.id)),
        );
        let detail = Detail {
            streaming_reply_id,
            session: thread.summary(),
            expired_approval: None,
            messages,
            older_messages: turns.next_cursor.is_some(),
            approvals: events
                .approvals
                .values()
                .filter(|request| request.thread == id.to_string())
                .map(|request| request.approval.clone())
                .collect(),
            error: turns
                .data
                .first()
                .and_then(|turn| turn.error.as_ref())
                .map(|error| public_error(&error.message)),
        };
        Ok((detail, rpc))
    }

    pub async fn approve(&self, id: Uuid, approval: Uuid, decision: Decision) -> Result<Value> {
        let _guard = self.mutation.lock().await;
        let mut rpc = self.connection().await?;
        let thread = self.metadata(&rpc, id).await?;
        if decision == Decision::Accept {
            self.verify(&rpc, &thread.cwd).await?;
        }
        let known = self
            .events
            .lock()
            .expect("Codex event lock")
            .approvals
            .get(&approval)
            .is_some_and(|request| request.thread == id.to_string());
        if !known {
            let (detail, restored) = self.read_with_rpc(id).await?;
            if decision == Decision::Accept && !rpc.sender.same_channel(&restored.sender) {
                self.verify(&restored, &detail.session.cwd).await?;
            }
            rpc = restored;
        }
        let request = {
            let events = self.events.lock().expect("Codex event lock");
            let pending = events
                .approvals
                .get(&approval)
                .filter(|request| request.thread == id.to_string())
                .ok_or_else(|| {
                    if rpc.sender.is_closed() {
                        disconnected()
                    } else {
                        Failure::new(409, "This approval was already resolved.")
                    }
                })?;
            if decision == Decision::Accept && !pending.approval.can_accept {
                return Err(Failure::new(400, "Review this request on the device."));
            }
            pending.request.clone()
        };
        let result = rpc
            .write(json!({"id":request,"result":{"decision":decision}}))
            .await;
        let mut events = self.events.lock().expect("Codex event lock");
        if let Err(error) = result.and_then(|()| {
            if rpc.sender.is_closed() {
                Err(disconnected())
            } else {
                Ok(())
            }
        }) {
            events.tracked.remove(&id.to_string());
            return Err(error);
        }
        events.approvals.remove(&approval);
        events.tracked.insert(id.to_string());
        Ok(json!({"resolved":true}))
    }

    pub async fn interrupt(&self, id: Uuid) -> Result<Value> {
        let rpc = self.connection().await?;
        self.metadata(&rpc, id).await?;
        let turns: Page<Turn> = rpc
            .request(
                "thread/turns/list",
                json!({"threadId":id,"limit":1,"sortDirection":"desc"}),
            )
            .await?;
        let Some(turn) = turns.data.iter().find(|turn| turn.status == "inProgress") else {
            return Ok(json!({"stopped":false}));
        };
        let _: Value = rpc
            .request("turn/interrupt", json!({"threadId":id,"turnId":turn.id}))
            .await?;
        // The interrupt reply does not include the finalized items. Drop the
        // partial stream now so the next read uses the saved turn, including
        // when the turn/completed notification was lost.
        self.events
            .lock()
            .expect("Codex event lock")
            .finish_turn(&id.to_string(), &turn.id);
        Ok(json!({"stopped":true}))
    }
}

pub fn visible(item: &Value, turn: &str) -> Option<Message> {
    let (role, text) = match item["type"].as_str()? {
        "userMessage" => (
            "user",
            item["content"]
                .as_array()?
                .iter()
                .filter(|part| part["type"] == "text")
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        "agentMessage" => ("assistant", item["text"].as_str()?.into()),
        "commandExecution" => (
            "activity",
            item["command"]
                .as_str()
                .unwrap_or("Running a command")
                .into(),
        ),
        "fileChange" => (
            "activity",
            format!(
                "Edited {}",
                item["changes"]
                    .as_array()?
                    .iter()
                    .filter_map(|change| change["path"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ),
        _ => return None,
    };
    (!text.is_empty()).then(|| Message {
        id: format!("{turn}/{}", item["id"].as_str().unwrap_or("")),
        role: role.into(),
        text: redact(&text).chars().take(256_000).collect(),
    })
}
