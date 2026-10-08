use std::{
    collections::{BTreeMap, HashMap},
    future::Future,
    io::ErrorKind,
    path::PathBuf,
    process::ExitCode,
    sync::Arc,
    time::Duration,
};

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ElicitRequest,
        ElicitRequestParams, ElicitResult, ElicitationAction, ElicitationSchema, Implementation,
        InputRequest, InputRequiredResult, JsonObject, ListToolsResult, PaginatedRequestParams,
        ResultType, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
    },
    service::RequestContext,
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::{
        unix::{OwnedReadHalf, OwnedWriteHalf},
        UnixStream,
    },
    sync::Mutex,
};
use uuid::Uuid;

use crate::{ai_tools::credential::LAUNCH_AGENT_LABEL, mcp::TOOL_NAMES};

pub const SERVER_INFO_NAME: &str = "litellm-relay";
pub const CONFIRMATION_TTL: Duration = Duration::from_secs(600);
const CONFIRMATION_KEY: &str = "confirm";

#[derive(Clone)]
pub struct RelayServer {
    socket: PathBuf,
    confirmation_ttl: Duration,
    awaiting_confirmation: Arc<Mutex<HashMap<String, Conversation>>>,
}

struct Conversation {
    op: String,
    arguments: Option<JsonObject>,
    writer: OwnedWriteHalf,
    lines: Lines<BufReader<OwnedReadHalf>>,
}

#[derive(Deserialize)]
struct Ask {
    message: String,
}

#[derive(Deserialize)]
struct DaemonLine {
    ask: Option<Ask>,
    ok: Option<bool>,
    result: Option<Value>,
    reason: Option<String>,
    message: Option<String>,
}

enum Step {
    Ask(String),
    Done(Value),
    Refused(String, String),
    Unknown,
}

impl From<DaemonLine> for Step {
    fn from(line: DaemonLine) -> Self {
        match line {
            DaemonLine { ask: Some(ask), .. } => Step::Ask(ask.message),
            DaemonLine {
                ok: Some(true),
                result: Some(result),
                ..
            } => Step::Done(result),
            DaemonLine {
                ok: Some(false),
                reason: Some(reason),
                message: Some(message),
                ..
            } => Step::Refused(reason, message),
            _ => Step::Unknown,
        }
    }
}

fn refused(reason: &str, message: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!("{reason}: {message}"))])
}

fn schema(properties: Value, required: &[&str]) -> JsonObject {
    let mut object = JsonObject::new();
    object.insert("type".into(), json!("object"));
    object.insert("properties".into(), properties);
    object.insert("required".into(), json!(required));
    object.insert("additionalProperties".into(), json!(false));
    object
}

pub fn meta_tools() -> Vec<Tool> {
    vec![
        Tool::new(
            "search_tools",
            "Find MCP tools on the LiteLLM Gateway by keyword. Returns at most five tool names from the servers the user activated, ranked by how many query words match the tool name, its server, and its description, plus the names of inactive servers that have matching tools (activate one with activate_server to search and call its tools). Call describe_tool on a name before calling it.",
            schema(
                json!({"query": {"type": "string", "description": "Keywords describing what the tool should do, such as 'create github issue'"}}),
                &["query"],
            ),
        )
        .annotate(ToolAnnotations::from_raw(
            Some("Search tools".into()),
            Some(true),
            Some(false),
            Some(true),
            Some(false),
        )),
        Tool::new(
            "describe_tool",
            "Show one Gateway MCP tool in full: its name, server, description, input schema, upstream annotations, and the verdict (allow runs with no prompt, ask prompts the user to confirm each call). The tool's server must be active.",
            schema(
                json!({"name": {"type": "string", "description": "The exact tool name as returned by search_tools"}}),
                &["name"],
            ),
        )
        .annotate(ToolAnnotations::from_raw(
            Some("Describe tool".into()),
            Some(true),
            Some(false),
            Some(true),
            Some(false),
        )),
        Tool::new(
            "call_tool",
            "Run one Gateway MCP tool with the given arguments and return its result unchanged. A tool the relay could not show to be read-only is run only after the user confirms it in a dialog; a declined or cancelled confirmation returns an error and runs nothing. Nothing you pass as an argument counts as that confirmation.",
            schema(
                json!({
                    "name": {"type": "string", "description": "The exact tool name as returned by search_tools"},
                    "arguments": {"type": "object", "description": "The arguments matching the input schema from describe_tool", "additionalProperties": true}
                }),
                &["name"],
            ),
        )
        .annotate(ToolAnnotations::from_raw(
            Some("Call tool".into()),
            Some(false),
            None,
            None,
            Some(true),
        )),
        Tool::new(
            "activate_server",
            "Make an MCP server's tools available to search_tools, describe_tool, and call_tool for this signed-in session. The user confirms the activation in a dialog; a declined or cancelled confirmation leaves the server inactive. Use the server names that search_tools lists under inactive_servers.",
            schema(
                json!({"server": {"type": "string", "description": "The server name as listed by search_tools"}}),
                &["server"],
            ),
        )
        .annotate(ToolAnnotations::from_raw(
            Some("Activate server".into()),
            Some(false),
            Some(false),
            Some(true),
            Some(false),
        )),
        Tool::new(
            "switch_team",
            "Switch the team that this user's Gateway spend is attributed to, for every client on the device. A call without an argument lists the current team, environment, and Gateway URL, the teams this user may pick, and the configured environments, and asks nothing. With team set, the user confirms the switch in a dialog; a declined or cancelled confirmation switches nothing. The result carries the new team and key expiry plus the same selection.",
            schema(
                json!({"team": {"type": "string", "description": "The team id to switch to, as listed under teams by a call without an argument"}}),
                &[],
            ),
        )
        .annotate(ToolAnnotations::from_raw(
            Some("Switch team".into()),
            Some(false),
            Some(false),
            Some(true),
            Some(false),
        )),
        Tool::new(
            "switch_environment",
            "Point every client on the device at another configured Gateway environment, keeping the sign-in. A call without an argument lists the current environment and the configured environments with their URLs, and asks nothing. With environment set, the user confirms the switch in a dialog; a declined or cancelled confirmation switches nothing. The result carries the new environment and Gateway URL plus the same selection.",
            schema(
                json!({"environment": {"type": "string", "description": "The environment name to switch to, as listed under environments by a call without an argument"}}),
                &[],
            ),
        )
        .annotate(ToolAnnotations::from_raw(
            Some("Switch environment".into()),
            Some(false),
            Some(false),
            Some(true),
            Some(false),
        )),
    ]
}

impl Conversation {
    async fn open(
        socket: &PathBuf,
        op: &str,
        arguments: Option<JsonObject>,
    ) -> Result<Self, CallToolResult> {
        let mut request: Map<String, Value> = arguments.clone().unwrap_or_default();
        request.insert("op".into(), json!(op));
        let stream = match UnixStream::connect(socket).await {
            Ok(stream) => stream,
            Err(error) if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused
            ) =>
            {
                return Err(refused(
                    "daemon_unavailable",
                    &format!(
                        "the Relay daemon is not running (no socket at {}); start it with `relay serve` or load the {LAUNCH_AGENT_LABEL} LaunchAgent",
                        socket.display()
                    ),
                ))
            }
            Err(error) => {
                return Err(refused(
                    "daemon_unavailable",
                    &format!("could not reach the daemon socket: {error}"),
                ))
            }
        };
        let (reader, mut writer) = stream.into_split();
        writer
            .write_all(format!("{}\n", Value::Object(request)).as_bytes())
            .await
            .map_err(|error| {
                refused(
                    "daemon_unavailable",
                    &format!("could not send the request: {error}"),
                )
            })?;
        Ok(Self {
            op: op.to_string(),
            arguments,
            writer,
            lines: BufReader::new(reader).lines(),
        })
    }

    async fn next(&mut self) -> Result<Step, CallToolResult> {
        let line = match self.lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                return Err(refused(
                    "daemon_unavailable",
                    "the daemon closed the connection without a reply",
                ))
            }
            Err(error) => {
                return Err(refused(
                    "daemon_unavailable",
                    &format!("no answer from the daemon: {error}"),
                ))
            }
        };
        match serde_json::from_str::<DaemonLine>(line.trim()).map(Step::from) {
            Ok(Step::Unknown) | Err(_) => Err(refused(
                "daemon_unavailable",
                "the daemon answered with something other than a known JSON line",
            )),
            Ok(step) => Ok(step),
        }
    }

    async fn answer(&mut self, answer: &str) -> Result<(), CallToolResult> {
        self.writer
            .write_all(format!("{}\n", json!({ "answer": answer })).as_bytes())
            .await
            .map_err(|error| {
                refused(
                    "daemon_unavailable",
                    &format!("could not send the user's answer: {error}"),
                )
            })
    }

    fn finish(&self, result: Value) -> CallToolResult {
        if self.op != "call_tool" {
            return CallToolResult::structured(result);
        }
        match serde_json::from_value::<CallToolResult>(result) {
            Ok(mut passed) => {
                passed.result_type.get_or_insert(ResultType::COMPLETE);
                passed
            }
            Err(error) => refused(
                "upstream_error",
                &format!("the daemon's tool result could not be read: {error}"),
            ),
        }
    }

    fn continues(&self, request: &CallToolRequestParams) -> bool {
        self.op == request.name && self.arguments == request.arguments
    }
}

impl RelayServer {
    pub fn new(socket: PathBuf) -> Self {
        Self {
            socket,
            confirmation_ttl: CONFIRMATION_TTL,
            awaiting_confirmation: Arc::default(),
        }
    }

    #[cfg(test)]
    fn with_confirmation_ttl(self, confirmation_ttl: Duration) -> Self {
        Self {
            confirmation_ttl,
            ..self
        }
    }

    async fn relay(
        &self,
        request: CallToolRequestParams,
        context: &RequestContext<RoleServer>,
    ) -> CallToolResponse {
        if let Some(state) = request.request_state.clone() {
            return self.resume(request, &state, context).await;
        }
        match Conversation::open(&self.socket, &request.name, request.arguments).await {
            Ok(conversation) => self.converse(conversation, context).await,
            Err(result) => CallToolResponse::from(result),
        }
    }

    async fn resume(
        &self,
        request: CallToolRequestParams,
        state: &str,
        context: &RequestContext<RoleServer>,
    ) -> CallToolResponse {
        let Some(mut conversation) = self.awaiting_confirmation.lock().await.remove(state) else {
            return CallToolResponse::from(refused(
                "confirmation_expired",
                "no confirmation is waiting under this request state (it was answered, expired, or never issued); call the tool again",
            ));
        };
        if !conversation.continues(&request) {
            return CallToolResponse::from(refused(
                "confirmation_mismatch",
                "the continued call names another tool or other arguments than the ones the user was asked to confirm; nothing was run",
            ));
        }
        let answer = request
            .input_responses
            .and_then(|mut responses| responses.remove(CONFIRMATION_KEY))
            .and_then(|response| serde_json::from_value::<ElicitResult>(response).ok())
            .map(answer_from)
            .unwrap_or("cancel");
        if let Err(result) = conversation.answer(answer).await {
            return CallToolResponse::from(result);
        }
        self.converse(conversation, context).await
    }

    async fn converse(
        &self,
        mut conversation: Conversation,
        context: &RequestContext<RoleServer>,
    ) -> CallToolResponse {
        loop {
            let step = match conversation.next().await {
                Ok(step) => step,
                Err(result) => return CallToolResponse::from(result),
            };
            match step {
                Step::Ask(_) if !client_can_ask(context) => {
                    if let Err(result) = conversation.answer("unsupported").await {
                        return CallToolResponse::from(result);
                    }
                }
                Step::Ask(message) if inline_lifecycle(context) => {
                    return self.park(conversation, message).await;
                }
                Step::Ask(message) => {
                    let answer = ask_through_the_peer(message, context).await;
                    if let Err(result) = conversation.answer(answer).await {
                        return CallToolResponse::from(result);
                    }
                }
                Step::Done(result) => return CallToolResponse::from(conversation.finish(result)),
                Step::Refused(reason, message) => {
                    return CallToolResponse::from(refused(&reason, &message))
                }
                Step::Unknown => {
                    return CallToolResponse::from(refused(
                        "daemon_unavailable",
                        "the daemon answered with something other than a known JSON line",
                    ))
                }
            }
        }
    }

    async fn park(&self, conversation: Conversation, message: String) -> CallToolResponse {
        let state = Uuid::new_v4().simple().to_string();
        self.awaiting_confirmation
            .lock()
            .await
            .insert(state.clone(), conversation);
        let awaiting = Arc::clone(&self.awaiting_confirmation);
        let (expiring, ttl) = (state.clone(), self.confirmation_ttl);
        tokio::spawn(async move {
            tokio::time::sleep(ttl).await;
            awaiting.lock().await.remove(&expiring);
        });
        let input_requests = BTreeMap::from([(
            CONFIRMATION_KEY.to_string(),
            InputRequest::Elicitation(ElicitRequest::new(confirmation(message))),
        )]);
        CallToolResponse::InputRequired(InputRequiredResult::new(Some(input_requests), Some(state)))
    }
}

fn client_can_ask(context: &RequestContext<RoleServer>) -> bool {
    context
        .client_capabilities()
        .is_some_and(|capabilities| capabilities.elicitation.is_some())
}

fn inline_lifecycle(context: &RequestContext<RoleServer>) -> bool {
    context
        .protocol_version()
        .is_some_and(|version| !version.has_initialize())
}

fn confirmation(message: String) -> ElicitRequestParams {
    let requested_schema = ElicitationSchema::builder()
        .required_bool_property(CONFIRMATION_KEY, |confirm| {
            confirm.title("Confirm").description(message.clone())
        })
        .build_unchecked();
    ElicitRequestParams::FormElicitationParams {
        meta: None,
        message,
        requested_schema,
    }
}

async fn ask_through_the_peer(
    message: String,
    context: &RequestContext<RoleServer>,
) -> &'static str {
    match context.peer.create_elicitation(confirmation(message)).await {
        Ok(result) => answer_from(result),
        Err(_) => "unsupported",
    }
}

fn answer_from(result: ElicitResult) -> &'static str {
    let confirmed = result
        .content
        .as_ref()
        .and_then(|content| content.get(CONFIRMATION_KEY))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    match result.action {
        ElicitationAction::Accept if confirmed => "accept",
        ElicitationAction::Accept | ElicitationAction::Decline => "decline",
        _ => "cancel",
    }
}

impl ServerHandler for RelayServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                SERVER_INFO_NAME,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "Tools on the LiteLLM Gateway, reached through the Relay daemon under the signed-in user. Start with search_tools, read describe_tool before call_tool, and activate a server named under inactive_servers with activate_server when its tools are needed.",
            )
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(meta_tools())))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        if !TOOL_NAMES.contains(&request.name.as_ref()) {
            return Err(McpError::invalid_params(
                format!(
                    "no tool is named {}; the tools are {}",
                    request.name,
                    TOOL_NAMES.join(", ")
                ),
                None,
            ));
        }
        Ok(self.relay(request, &context).await)
    }
}

pub async fn run_mcp(socket: PathBuf) -> ExitCode {
    let service = match RelayServer::new(socket)
        .serve(rmcp::transport::stdio())
        .await
    {
        Ok(service) => service,
        Err(error) => {
            eprintln!("relay mcp: the MCP client did not initialize: {error}");
            return ExitCode::FAILURE;
        }
    };
    match service.waiting().await {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("relay mcp: the MCP connection ended with an error: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use rmcp::{
        model::{
            ClientCapabilities, ClientConfig, ElicitRequestParams, ElicitResult, ElicitationAction,
            Implementation, PaginatedRequestParams, ProtocolVersion,
        },
        service::{RoleClient, RunningService},
        ClientHandler, ClientLifecycleMode, ClientServiceExt,
    };
    use tokio::{
        io::{DuplexStream, ReadHalf, WriteHalf},
        net::UnixListener,
        sync::Barrier,
    };
    use uuid::Uuid;

    use super::*;

    struct StandIn {
        requests: Arc<Mutex<Vec<Value>>>,
        answers: Arc<Mutex<Vec<String>>>,
        barrier: Arc<Barrier>,
    }

    impl StandIn {
        fn serve(path: &PathBuf, slow_callers: usize) -> Self {
            let listener = UnixListener::bind(path).expect("bind");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let answers = Arc::new(Mutex::new(Vec::new()));
            let barrier = Arc::new(Barrier::new(slow_callers));
            let (seen, heard, gate) = (
                Arc::clone(&requests),
                Arc::clone(&answers),
                Arc::clone(&barrier),
            );
            tokio::spawn(async move {
                loop {
                    let (stream, _) = listener.accept().await.expect("accept");
                    let (seen, heard, gate) =
                        (Arc::clone(&seen), Arc::clone(&heard), Arc::clone(&gate));
                    tokio::spawn(async move {
                        let (reader, mut writer) = stream.into_split();
                        let mut lines = BufReader::new(reader).lines();
                        let request: Value =
                            serde_json::from_str(&lines.next_line().await.unwrap().unwrap())
                                .unwrap();
                        seen.lock().unwrap().push(request.clone());
                        let reply = match (request["op"].as_str(), request["name"].as_str()) {
                            (Some("search_tools"), _) if request["query"] == "slow" => {
                                gate.wait().await;
                                json!({"ok": true, "result": {"tools": [], "inactive_servers": []}})
                            }
                            (Some("search_tools"), _) => {
                                json!({"ok": true, "result": {"tools": ["github-list_issues"], "inactive_servers": ["jira"]}})
                            }
                            (Some("switch_team"), _) => {
                                json!({"ok": true, "result": {"team": "team-a", "environment": "dev", "gateway_url": "https://gateway.example.com", "teams": [{"id": "team-a", "alias": "Team A"}], "teams_error": null, "environments": [{"name": "dev", "url": "https://gateway.example.com"}]}})
                            }
                            (Some("call_tool"), Some("github-get_issue")) => {
                                json!({"ok": true, "result": {"content": [{"type": "text", "text": "issue 1"}], "structuredContent": {"number": 1}, "isError": true}})
                            }
                            (Some("call_tool"), Some("github-create_issue")) => {
                                writer
                                    .write_all(
                                        format!(
                                            "{}\n",
                                            json!({"ask": {"kind": "confirm_call", "message": "Run github-create_issue on the MCP server github?"}})
                                        )
                                        .as_bytes(),
                                    )
                                    .await
                                    .unwrap();
                                let answer = match lines.next_line().await.unwrap() {
                                    Some(line) => serde_json::from_str::<Value>(&line).unwrap()
                                        ["answer"]
                                        .as_str()
                                        .unwrap()
                                        .to_string(),
                                    None => "closed".to_string(),
                                };
                                heard.lock().unwrap().push(answer.clone());
                                if answer == "closed" {
                                    return;
                                }
                                match answer.as_str() {
                                    "accept" => {
                                        json!({"ok": true, "result": {"content": [{"type": "text", "text": "ran"}], "isError": false}})
                                    }
                                    "decline" => {
                                        json!({"ok": false, "reason": "declined", "message": "the user declined; nothing was run"})
                                    }
                                    "cancel" => {
                                        json!({"ok": false, "reason": "cancelled", "message": "the confirmation was cancelled; nothing was run"})
                                    }
                                    _ => {
                                        json!({"ok": false, "reason": "confirmation_required", "message": "the client cannot ask"})
                                    }
                                }
                            }
                            _ => {
                                json!({"ok": false, "reason": "unknown_tool", "message": "no tool is named nope"})
                            }
                        };
                        writer
                            .write_all(format!("{reply}\n").as_bytes())
                            .await
                            .unwrap();
                        writer.shutdown().await.unwrap();
                    });
                }
            });
            Self {
                requests,
                answers,
                barrier,
            }
        }
    }

    #[derive(Clone)]
    struct FakeClient {
        can_ask: bool,
        answer: (ElicitationAction, Option<bool>),
        asked: Arc<Mutex<Vec<ElicitRequestParams>>>,
    }

    impl FakeClient {
        fn answering(action: ElicitationAction, confirm: Option<bool>) -> Self {
            Self {
                can_ask: true,
                answer: (action, confirm),
                asked: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn without_elicitation() -> Self {
            Self {
                can_ask: false,
                ..Self::answering(ElicitationAction::Accept, Some(true))
            }
        }
    }

    impl ClientHandler for FakeClient {
        fn get_info(&self) -> ClientConfig {
            let capabilities = if self.can_ask {
                ClientCapabilities::builder().enable_elicitation().build()
            } else {
                ClientCapabilities::default()
            };
            ClientConfig::new(capabilities, Implementation::new("fake-client", "0"))
        }

        fn create_elicitation(
            &self,
            request: ElicitRequestParams,
            _context: RequestContext<RoleClient>,
        ) -> impl Future<Output = Result<ElicitResult, McpError>> + Send + '_ {
            self.asked.lock().unwrap().push(request);
            let (action, confirm) = self.answer.clone();
            let result = match confirm {
                Some(confirm) => {
                    ElicitResult::new(action).with_content(json!({ "confirm": confirm }))
                }
                None => ElicitResult::new(action),
            };
            std::future::ready(Ok(result))
        }
    }

    fn socket_in_temp_dir() -> PathBuf {
        let dir = env::temp_dir().join(format!("rmcp-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join("mcp.sock")
    }

    async fn connect(
        socket: PathBuf,
        client: FakeClient,
    ) -> RunningService<RoleClient, FakeClient> {
        connect_with(socket, client, ClientLifecycleMode::Initialize).await
    }

    async fn connect_with(
        socket: PathBuf,
        client: FakeClient,
        lifecycle: ClientLifecycleMode,
    ) -> RunningService<RoleClient, FakeClient> {
        let (server_side, client_side) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let server = RelayServer::new(socket)
                .serve(server_side)
                .await
                .expect("server");
            let _ = server.waiting().await;
        });
        client
            .serve_with_lifecycle(client_side, lifecycle)
            .await
            .expect("client")
    }

    fn discover_lifecycle() -> ClientLifecycleMode {
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        }
    }

    struct Wire {
        writer: WriteHalf<DuplexStream>,
        lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
        next_id: u64,
        server_sent: Vec<Value>,
    }

    impl Wire {
        fn open(server: RelayServer) -> Self {
            let (server_side, client_side) = tokio::io::duplex(64 * 1024);
            tokio::spawn(async move {
                let running = server.serve(server_side).await.expect("server");
                let _ = running.waiting().await;
            });
            let (reader, writer) = tokio::io::split(client_side);
            Self {
                writer,
                lines: BufReader::new(reader).lines(),
                next_id: 0,
                server_sent: Vec::new(),
            }
        }

        async fn request(&mut self, method: &str, params: Value) -> Value {
            let id = self.next_id;
            self.next_id += 1;
            let line = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
            self.writer
                .write_all(format!("{line}\n").as_bytes())
                .await
                .expect("write");
            loop {
                let line = tokio::time::timeout(Duration::from_secs(10), self.lines.next_line())
                    .await
                    .expect("a reply within 10 s")
                    .expect("readable")
                    .expect("an open connection");
                let message: Value = serde_json::from_str(&line).expect("json");
                if message["id"] == json!(id) && message.get("method").is_none() {
                    return message["result"].clone();
                }
                self.server_sent.push(message);
            }
        }

        async fn call(&mut self, arguments: Value, continuation: Option<(&str, Value)>) -> Value {
            let mut params = json!({
                "_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {"elicitation": {"form": {}}}
                },
                "name": "call_tool",
                "arguments": arguments,
            });
            if let Some((state, response)) = continuation {
                params["requestState"] = json!(state);
                params["inputResponses"] = json!({ "confirm": response });
            }
            self.request("tools/call", params).await
        }
    }

    async fn discovered_wire(server: RelayServer) -> Wire {
        let mut wire = Wire::open(server);
        let discovered = wire
            .request(
                "server/discover",
                json!({"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {"elicitation": {"form": {}}}
                }}),
            )
            .await;
        assert!(discovered["supportedVersions"]
            .as_array()
            .expect("supported versions")
            .contains(&json!("2026-07-28")));
        assert_eq!(
            discovered["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            SERVER_INFO_NAME
        );
        wire
    }

    fn create_issue() -> Value {
        json!({"name": "github-create_issue", "arguments": {"title": "x"}})
    }

    async fn eventually<T: PartialEq + std::fmt::Debug>(expected: T, read: impl Fn() -> T) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while read() != expected {
            assert!(
                tokio::time::Instant::now() < deadline,
                "expected {expected:?}, last saw {:?}",
                read()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn call(
        client: &RunningService<RoleClient, FakeClient>,
        name: &str,
        arguments: Value,
    ) -> CallToolResult {
        let arguments = arguments.as_object().cloned().unwrap_or_default();
        client
            .call_tool(CallToolRequestParams::new(name.to_string()).with_arguments(arguments))
            .await
            .expect("tool result")
    }

    fn text_of(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|text| text.text.clone()))
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_list_the_six_meta_tools_with_truthful_annotations() {
        let socket = socket_in_temp_dir();
        let client = connect(socket, FakeClient::without_elicitation()).await;
        let info = client.peer_info().expect("server info");
        let implementation = info.server_info.as_ref().expect("implementation");
        assert_eq!(implementation.name, "litellm-relay");
        assert_eq!(implementation.version, env!("CARGO_PKG_VERSION"));
        let tools = client.list_all_tools().await.expect("tools");
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        assert_eq!(names, TOOL_NAMES);
        for tool in &tools {
            let annotations = tool.annotations.as_ref().expect("annotations");
            let expected_read_only = matches!(tool.name.as_ref(), "search_tools" | "describe_tool");
            assert_eq!(
                annotations.read_only_hint,
                Some(expected_read_only),
                "{}",
                tool.name
            );
            if tool.name == "call_tool" {
                assert_eq!(annotations.open_world_hint, Some(true));
            }
            assert!(tool
                .description
                .as_ref()
                .is_some_and(|text| !text.is_empty()));
        }
        let search = tools
            .iter()
            .find(|tool| tool.name == "search_tools")
            .unwrap();
        assert!(search
            .description
            .as_ref()
            .unwrap()
            .contains("at most five"));
        assert_eq!(search.input_schema["required"], json!(["query"]));
        for (name, argument) in [
            ("switch_team", "team"),
            ("switch_environment", "environment"),
        ] {
            let switch = tools.iter().find(|tool| tool.name == name).unwrap();
            assert_eq!(switch.input_schema["required"], json!([]));
            assert_eq!(
                switch.input_schema["properties"][argument]["type"],
                "string"
            );
            let annotations = switch.annotations.as_ref().unwrap();
            assert_eq!(annotations.destructive_hint, Some(false));
            assert_eq!(annotations.idempotent_hint, Some(true));
            assert!(switch
                .description
                .as_ref()
                .unwrap()
                .contains("without an argument"));
        }
        let _ = client
            .list_tools(Some(PaginatedRequestParams::default()))
            .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_answer_search_and_pass_a_tool_result_through_unchanged() {
        let socket = socket_in_temp_dir();
        let stand_in = StandIn::serve(&socket, 1);
        let client = connect(socket, FakeClient::without_elicitation()).await;

        let found = call(&client, "search_tools", json!({"query": "issues"})).await;
        assert_eq!(found.is_error, Some(false));
        let expected = json!({"tools": ["github-list_issues"], "inactive_servers": ["jira"]});
        assert_eq!(found.structured_content, Some(expected.clone()));
        assert_eq!(
            serde_json::from_str::<Value>(&text_of(&found)).unwrap(),
            expected
        );
        assert_eq!(
            stand_in.requests.lock().unwrap()[0],
            json!({"op": "search_tools", "query": "issues"})
        );

        let passed = call(
            &client,
            "call_tool",
            json!({"name": "github-get_issue", "arguments": {"number": 1}}),
        )
        .await;
        assert_eq!(passed.is_error, Some(true));
        assert_eq!(text_of(&passed), "issue 1");
        assert_eq!(passed.structured_content, Some(json!({"number": 1})));
        assert_eq!(
            stand_in.requests.lock().unwrap()[1],
            json!({"op": "call_tool", "name": "github-get_issue", "arguments": {"number": 1}})
        );

        let refused = call(&client, "describe_tool", json!({"name": "nope"})).await;
        assert_eq!(refused.is_error, Some(true));
        assert_eq!(text_of(&refused), "unknown_tool: no tool is named nope");
        assert!(refused.structured_content.is_none());

        let listed = call(&client, "switch_team", json!({})).await;
        assert_eq!(listed.is_error, Some(false));
        assert_eq!(
            listed.structured_content.as_ref().unwrap()["team"],
            "team-a"
        );
        assert_eq!(
            listed.structured_content.as_ref().unwrap()["environments"],
            json!([{"name": "dev", "url": "https://gateway.example.com"}])
        );
        assert_eq!(
            stand_in.requests.lock().unwrap()[3],
            json!({"op": "switch_team"})
        );

        let unknown = client
            .call_tool(CallToolRequestParams::new("forget_tool".to_string()))
            .await;
        assert!(unknown.is_err());
        assert_eq!(stand_in.requests.lock().unwrap().len(), 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_relay_one_elicitation_and_map_the_client_action_to_the_answer_line() {
        for (client, expected_answer, reason) in [
            (
                FakeClient::answering(ElicitationAction::Accept, Some(true)),
                "accept",
                None,
            ),
            (
                FakeClient::answering(ElicitationAction::Accept, Some(false)),
                "decline",
                Some("declined"),
            ),
            (
                FakeClient::answering(ElicitationAction::Accept, None),
                "decline",
                Some("declined"),
            ),
            (
                FakeClient::answering(ElicitationAction::Decline, None),
                "decline",
                Some("declined"),
            ),
            (
                FakeClient::answering(ElicitationAction::Cancel, None),
                "cancel",
                Some("cancelled"),
            ),
        ] {
            let socket = socket_in_temp_dir();
            let stand_in = StandIn::serve(&socket, 1);
            let asked = Arc::clone(&client.asked);
            let running = connect(socket, client).await;
            let result = call(
                &running,
                "call_tool",
                json!({"name": "github-create_issue", "arguments": {"title": "x"}, "confirmed": true}),
            )
            .await;
            assert_eq!(*stand_in.answers.lock().unwrap(), vec![expected_answer]);
            let asked = asked.lock().unwrap();
            assert_eq!(asked.len(), 1);
            let ElicitRequestParams::FormElicitationParams {
                message,
                requested_schema,
                ..
            } = &asked[0]
            else {
                panic!("expected a form elicitation");
            };
            assert_eq!(message, "Run github-create_issue on the MCP server github?");
            let schema = serde_json::to_value(requested_schema).unwrap();
            assert_eq!(schema["required"], json!(["confirm"]));
            assert_eq!(schema["properties"]["confirm"]["type"], "boolean");
            assert_eq!(schema["properties"]["confirm"]["title"], "Confirm");
            match reason {
                None => {
                    assert_eq!(result.is_error, Some(false));
                    assert_eq!(text_of(&result), "ran");
                }
                Some(reason) => {
                    assert_eq!(result.is_error, Some(true));
                    assert!(text_of(&result).starts_with(&format!("{reason}: ")));
                }
            }
        }

        let socket = socket_in_temp_dir();
        let stand_in = StandIn::serve(&socket, 1);
        let client = FakeClient::without_elicitation();
        let asked = Arc::clone(&client.asked);
        let running = connect(socket, client).await;
        let result = call(
            &running,
            "call_tool",
            json!({"name": "github-create_issue", "arguments": {"title": "x"}}),
        )
        .await;
        assert_eq!(*stand_in.answers.lock().unwrap(), vec!["unsupported"]);
        assert!(asked.lock().unwrap().is_empty());
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            text_of(&result),
            "confirmation_required: the client cannot ask"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_read_the_elicitation_capability_from_each_request_under_the_discover_handshake()
    {
        let socket = socket_in_temp_dir();
        let stand_in = StandIn::serve(&socket, 1);
        let client = FakeClient::answering(ElicitationAction::Accept, Some(true));
        let asked = Arc::clone(&client.asked);
        let running = connect_with(socket, client, discover_lifecycle()).await;
        assert_eq!(
            running.peer_info().expect("server info").protocol_version,
            ProtocolVersion::V_2026_07_28
        );
        let result = call(
            &running,
            "call_tool",
            json!({"name": "github-create_issue", "arguments": {"title": "x"}}),
        )
        .await;
        assert_eq!(asked.lock().unwrap().len(), 1);
        assert_eq!(*stand_in.answers.lock().unwrap(), vec!["accept"]);
        assert_eq!(result.is_error, Some(false));
        assert_eq!(text_of(&result), "ran");

        let socket = socket_in_temp_dir();
        let stand_in = StandIn::serve(&socket, 1);
        let client = FakeClient::without_elicitation();
        let asked = Arc::clone(&client.asked);
        let running = connect_with(socket, client, discover_lifecycle()).await;
        let result = call(
            &running,
            "call_tool",
            json!({"name": "github-create_issue", "arguments": {"title": "x"}}),
        )
        .await;
        assert!(asked.lock().unwrap().is_empty());
        assert_eq!(*stand_in.answers.lock().unwrap(), vec!["unsupported"]);
        assert_eq!(
            text_of(&result),
            "confirmation_required: the client cannot ask"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_run_calls_concurrently_and_name_a_missing_daemon() {
        let socket = socket_in_temp_dir();
        let stand_in = StandIn::serve(&socket, 5);
        let client = Arc::new(connect(socket, FakeClient::without_elicitation()).await);
        let calls: Vec<_> = (0..5)
            .map(|_| {
                let client = Arc::clone(&client);
                tokio::spawn(async move {
                    call(&client, "search_tools", json!({"query": "slow"})).await
                })
            })
            .collect();
        for handle in calls {
            let result = tokio::time::timeout(Duration::from_secs(10), handle)
                .await
                .expect("five calls overlap instead of waiting on each other")
                .expect("call task");
            assert_eq!(result.is_error, Some(false));
        }
        assert_eq!(stand_in.requests.lock().unwrap().len(), 5);
        drop(stand_in.barrier);

        let missing = socket_in_temp_dir();
        let client = connect(missing.clone(), FakeClient::without_elicitation()).await;
        let result = call(&client, "search_tools", json!({"query": "issues"})).await;
        assert_eq!(result.is_error, Some(true));
        let text = text_of(&result);
        assert!(text.starts_with("daemon_unavailable: "));
        assert!(text.contains(&missing.display().to_string()));
        assert!(text.contains(LAUNCH_AGENT_LABEL));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_ask_through_an_input_required_result_and_never_a_request_of_its_own_under_the_inline_lifecycle(
    ) {
        for (response, expected_answer, expected_text, is_error) in [
            (
                json!({"action": "accept", "content": {"confirm": true}}),
                "accept",
                "ran",
                false,
            ),
            (
                json!({"action": "accept", "content": {"confirm": false}}),
                "decline",
                "declined: the user declined; nothing was run",
                true,
            ),
            (
                json!({"action": "decline"}),
                "decline",
                "declined: the user declined; nothing was run",
                true,
            ),
            (
                json!({"action": "cancel"}),
                "cancel",
                "cancelled: the confirmation was cancelled; nothing was run",
                true,
            ),
        ] {
            let socket = socket_in_temp_dir();
            let stand_in = StandIn::serve(&socket, 1);
            let mut wire = discovered_wire(RelayServer::new(socket)).await;

            let asked = wire.call(create_issue(), None).await;
            assert_eq!(asked["resultType"], "input_required");
            let request = &asked["inputRequests"]["confirm"];
            assert_eq!(request["method"], "elicitation/create");
            assert_eq!(
                request["params"]["message"],
                "Run github-create_issue on the MCP server github?"
            );
            assert_eq!(request["params"]["mode"], "form");
            let schema = &request["params"]["requestedSchema"];
            assert_eq!(schema["required"], json!(["confirm"]));
            assert_eq!(schema["properties"]["confirm"]["type"], "boolean");
            let state = asked["requestState"].as_str().expect("a request state");
            assert!(!state.is_empty());
            assert!(stand_in.answers.lock().unwrap().is_empty());

            let result = wire.call(create_issue(), Some((state, response))).await;
            assert_eq!(*stand_in.answers.lock().unwrap(), vec![expected_answer]);
            assert_eq!(result["isError"], json!(is_error));
            assert_eq!(result["content"][0]["text"], expected_text);
            assert!(
                wire.server_sent
                    .iter()
                    .all(|message| message.get("method").is_none()),
                "the relay sent a request of its own: {:?}",
                wire.server_sent
            );
            assert_eq!(stand_in.requests.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_refuse_a_continuation_whose_state_is_unknown_expired_or_for_another_call() {
        let socket = socket_in_temp_dir();
        let stand_in = StandIn::serve(&socket, 1);
        let mut wire = discovered_wire(RelayServer::new(socket)).await;
        let forged = wire
            .call(
                create_issue(),
                Some((
                    "not-a-state",
                    json!({"action": "accept", "content": {"confirm": true}}),
                )),
            )
            .await;
        assert_eq!(forged["isError"], json!(true));
        assert!(forged["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("confirmation_expired: "));
        assert!(stand_in.requests.lock().unwrap().is_empty());

        let asked = wire.call(create_issue(), None).await;
        let state = asked["requestState"].as_str().unwrap().to_string();
        let other = json!({"name": "github-create_issue", "arguments": {"title": "y"}});
        let mismatch = wire
            .call(
                other,
                Some((
                    &state,
                    json!({"action": "accept", "content": {"confirm": true}}),
                )),
            )
            .await;
        assert_eq!(mismatch["isError"], json!(true));
        assert!(mismatch["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("confirmation_mismatch: "));
        eventually(vec!["closed".to_string()], || {
            stand_in.answers.lock().unwrap().clone()
        })
        .await;
        let reused = wire
            .call(
                create_issue(),
                Some((
                    &state,
                    json!({"action": "accept", "content": {"confirm": true}}),
                )),
            )
            .await;
        assert!(reused["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("confirmation_expired: "));

        let socket = socket_in_temp_dir();
        let stand_in = StandIn::serve(&socket, 1);
        let server = RelayServer::new(socket).with_confirmation_ttl(Duration::from_millis(200));
        let mut wire = discovered_wire(server).await;
        let asked = wire.call(create_issue(), None).await;
        let state = asked["requestState"].as_str().unwrap().to_string();
        eventually(vec!["closed".to_string()], || {
            stand_in.answers.lock().unwrap().clone()
        })
        .await;
        let late = wire
            .call(
                create_issue(),
                Some((
                    &state,
                    json!({"action": "accept", "content": {"confirm": true}}),
                )),
            )
            .await;
        assert!(late["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("confirmation_expired: "));
        assert_eq!(stand_in.requests.lock().unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_mark_a_passed_through_tool_result_complete_under_the_inline_lifecycle() {
        let socket = socket_in_temp_dir();
        let stand_in = StandIn::serve(&socket, 1);
        let mut wire = discovered_wire(RelayServer::new(socket)).await;
        let passed = wire
            .call(
                json!({"name": "github-get_issue", "arguments": {"number": 1}}),
                None,
            )
            .await;
        assert_eq!(passed["resultType"], "complete");
        assert_eq!(passed["isError"], json!(true));
        assert_eq!(passed["content"][0]["text"], "issue 1");
        assert_eq!(passed["structuredContent"], json!({"number": 1}));
        assert_eq!(stand_in.requests.lock().unwrap().len(), 1);
    }
}
