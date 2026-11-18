use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message as Frame};
use url::Url;
use uuid::Uuid;

use super::{
    claude, codex,
    protocol::{self, Action, Agent, Decision, Outcome},
    route,
    service::Service,
    store::{RequestState, Store},
};

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("remote-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Server {
    url: Url,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(router: Router) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Server { url, task }
}

fn client() -> reqwest::Client {
    crate::cert::http_client_builder()
        .no_proxy()
        .default_headers(HeaderMap::from_iter([(
            "x-litellm-client".parse().unwrap(),
            "ios".parse().unwrap(),
        )]))
        .build()
        .unwrap()
}

async fn gateway() -> Server {
    async fn start(State(counter): State<Arc<AtomicUsize>>) -> Json<Value> {
        Json(
            json!({"login_id":counter.fetch_add(1, Ordering::SeqCst).to_string(),"poll_secret":"fixture-poll-secret","user_code":"FIXTURE","expires_in":600}),
        )
    }
    async fn poll(Path(id): Path<String>, headers: HeaderMap) -> Json<Value> {
        assert_eq!(headers["x-litellm-cli-poll-secret"], "fixture-poll-secret");
        Json(
            json!({"status":"ready","key":"fixture-gateway-key","user_id":format!("subject-{id}")}),
        )
    }
    async fn models(headers: HeaderMap) -> Json<Value> {
        assert_eq!(headers["authorization"], "Bearer fixture-gateway-key");
        Json(json!({"data":[{"id":"fixture-model"}]}))
    }
    serve(
        Router::new()
            .route("/sso/cli/start", post(start))
            .route("/sso/cli/poll/{id}", get(poll))
            .route("/v1/models", get(models))
            .with_state(Arc::new(AtomicUsize::new(0))),
    )
    .await
}

async fn control(gateway: Url) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let service = Service::open(gateway, url.clone(), std::path::Path::new(":memory:")).unwrap();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            service
                .router()
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Server { url, task }
}

async fn sign_in(server: &Server) -> String {
    let start: Value = client()
        .post(server.url.join("/api/auth/start").unwrap())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let poll: Value = client()
        .post(server.url.join("/api/auth/poll").unwrap())
        .json(&json!({"flowToken":start["flowToken"]}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    poll["token"].as_str().unwrap().into()
}

#[test]
fn persisted_identity_expiry_revocation_and_request_replay() {
    let directory = Directory::new();
    let path = directory.0.join("control.sqlite");
    let request = Uuid::new_v4();
    let (token, device_token, id, expires) = {
        let store = Store::open(&path).unwrap();
        let (token, identity) = store
            .issue("owner", &["fixture-model".into()], 100)
            .unwrap();
        let (device, device_token) = store.enroll("owner", "Mac", 100).unwrap();
        assert!(store.device(device.id, "other").is_err());
        assert!(matches!(
            store.begin("owner", request, "input", 100).unwrap(),
            RequestState::New
        ));
        assert!(matches!(
            store.begin("owner", request, "input", 101).unwrap(),
            RequestState::Unconfirmed
        ));
        assert!(store.begin("owner", request, "different", 101).is_err());
        store
            .finish("owner", request, Ok(json!({"turnId":"one-turn"})))
            .unwrap();
        (token, device_token, device.id, identity.expires_at)
    };
    let store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .identity(&token, expires - 1)
            .unwrap()
            .unwrap()
            .subject,
        "owner"
    );
    assert!(store.identity(&token, expires).unwrap().is_none());
    assert_eq!(store.models(&token).unwrap(), vec!["fixture-model"]);
    match store.begin("owner", request, "input", 102).unwrap() {
        RequestState::Cached(outcome) => {
            assert_eq!(outcome.into_result().unwrap()["turnId"], "one-turn")
        }
        _ => panic!("completed request was not cached"),
    }
    assert_eq!(
        store
            .authenticate_device(&device_token)
            .unwrap()
            .unwrap()
            .id,
        id
    );
    store.revoke(id, "other").unwrap();
    assert!(store.authenticate_device(&device_token).unwrap().is_some());
    store.revoke(id, "owner").unwrap();
    assert!(store.authenticate_device(&device_token).unwrap().is_none());
    store.logout(&token).unwrap();
    assert!(store.identity(&token, 102).unwrap().is_none());
    let db = rusqlite::Connection::open(path).unwrap();
    let saved: String = db
        .query_row(
            "SELECT response FROM requests WHERE request_id = ?",
            [request.to_string()],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!saved.contains(&token));
}

#[tokio::test]
async fn sso_owner_isolation_websocket_routing_and_mutation_deduplication() {
    let gateway = gateway().await;
    let server = control(gateway.url.clone()).await;
    let owner = sign_in(&server).await;
    let other = sign_in(&server).await;
    let device: Value = client()
        .post(server.url.join("/api/devices").unwrap())
        .bearer_auth(&owner)
        .json(&json!({"name":"QA Mac"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = device["deviceId"].as_str().unwrap();
    let path = server
        .url
        .join(&format!("/api/devices/{id}/sessions"))
        .unwrap();
    assert_eq!(
        client()
            .get(path.clone())
            .bearer_auth(&other)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        client().get(path.clone()).send().await.unwrap().status(),
        401
    );
    assert_eq!(
        client()
            .get(path.clone())
            .bearer_auth(&owner)
            .header("origin", "https://browser.example.test")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        client()
            .get(path.clone())
            .bearer_auth(&owner)
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    let input = json!({"requestId":Uuid::new_v4(),"cwd":"/fixture","agent":"claude"});
    for _ in 0..2 {
        assert_eq!(
            client()
                .post(path.clone())
                .bearer_auth(&owner)
                .json(&input)
                .send()
                .await
                .unwrap()
                .status(),
            503
        );
    }
    let mut endpoint = server.url.join("/agent").unwrap();
    endpoint.set_scheme("ws").unwrap();
    let mut request = endpoint.as_str().into_client_request().unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", device["deviceToken"].as_str().unwrap())
            .parse()
            .unwrap(),
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(request.clone())
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mac = tokio::spawn(async move {
        while let Some(Ok(frame)) = socket.next().await {
            match frame {
                Frame::Text(text) => {
                    let command: protocol::Command = serde_json::from_str(&text).unwrap();
                    assert!(matches!(
                        command.action,
                        Action::Create {
                            agent: Agent::Claude,
                            ..
                        }
                    ));
                    count.fetch_add(1, Ordering::SeqCst);
                    if matches!(&command.action, Action::Create { cwd, .. } if cwd == "/uncertain")
                    {
                        socket.close(None).await.unwrap();
                        return;
                    }
                    let reply = protocol::Reply {
                        id: command.id,
                        result: Outcome::from_result(Ok(json!({"id":"created-once"}))),
                    };
                    socket
                        .send(Frame::Text(serde_json::to_string(&reply).unwrap().into()))
                        .await
                        .unwrap();
                }
                Frame::Ping(bytes) => socket.send(Frame::Pong(bytes)).await.unwrap(),
                _ => {}
            }
        }
    });
    for _ in 0..2 {
        let response = client()
            .post(path.clone())
            .bearer_auth(&owner)
            .json(&input)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
        assert_eq!(
            response.json::<Value>().await.unwrap()["id"],
            "created-once"
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let uncertain = json!({"requestId":Uuid::new_v4(),"cwd":"/uncertain","agent":"claude"});
    assert_eq!(
        client()
            .post(path.clone())
            .bearer_auth(&owner)
            .json(&uncertain)
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let (_reconnected, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let retry = tokio::time::timeout(
        Duration::from_secs(2),
        client()
            .post(path.clone())
            .bearer_auth(&owner)
            .json(&uncertain)
            .send(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(retry.status(), 503);
    let denied = client()
        .post(
            server
                .url
                .join(&format!(
                    "/api/devices/{id}/sessions/claude:{}/messages",
                    Uuid::new_v4()
                ))
                .unwrap(),
        )
        .bearer_auth(&owner)
        .json(&json!({"requestId":Uuid::new_v4(),"text":"Hello","model":"unavailable-model"}))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    assert_eq!(
        client()
            .delete(server.url.join(&format!("/api/devices/{id}")).unwrap())
            .bearer_auth(&owner)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client()
            .get(path)
            .bearer_auth(&owner)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    mac.abort();
}

#[tokio::test]
async fn relay_route_uses_current_broker_gateway_and_never_sends_credentials() {
    assert_eq!(
        route::normalized(&Url::parse("https://gateway.example.test/v1/v1/").unwrap()),
        "https://gateway.example.test/v1"
    );
    let selected = Arc::new(std::sync::Mutex::new(
        "https://gateway.example.test".to_string(),
    ));
    async fn status(
        State(selected): State<Arc<std::sync::Mutex<String>>>,
        headers: HeaderMap,
    ) -> Json<Value> {
        assert!(!headers.contains_key("authorization"));
        Json(
            json!({"runtime":"rust","gateway_url":"https://stale.example.test","broker":{"gateway_url":selected.lock().unwrap().clone()},"account":{"gateway":{"url":"https://gateway.example.test"}}}),
        )
    }
    let server = serve(
        Router::new()
            .route("/api/status", get(status))
            .with_state(selected.clone()),
    )
    .await;
    let gateway = Url::parse("https://gateway.example.test/v1").unwrap();
    assert!(route::verify(&server.url.join("v1").unwrap(), &gateway)
        .await
        .unwrap());
    *selected.lock().unwrap() = "https://different.example.test".into();
    assert_eq!(
        route::verify(&server.url, &gateway)
            .await
            .unwrap_err()
            .status,
        409
    );
    assert!(!route::verify(&gateway, &gateway).await.unwrap());
    assert_eq!(
        route::verify(
            &Url::parse("https://user:secret@gateway.example.test/v1").unwrap(),
            &gateway
        )
        .await
        .unwrap_err()
        .status,
        409
    );
    assert_eq!(
        route::verify(&server.url.join("unrelated").unwrap(), &gateway)
            .await
            .unwrap_err()
            .status,
        409
    );
}

#[test]
fn only_public_messages_are_exposed_and_credentials_are_redacted() {
    let secret = format!("sk-{}", "a".repeat(30));
    let messages = claude::visible(
        &json!({"id":"assistant-message","content":[{"type":"thinking","thinking":"hidden"},{"type":"text","text":format!("Visible {secret}")},{"type":"tool_use","id":"tool","name":"Edit","input":{"file_path":"src/main.rs"}}]}),
        "uuid",
        "assistant",
    );
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].text, "Visible [credential redacted]");
    assert_eq!(messages[1].role, "activity");
    assert!(codex::visible(
        &json!({"id":"reasoning","type":"reasoning","text":"hidden"}),
        "turn"
    )
    .is_none());
    assert!(codex::visible(
        &json!({"id":"output","type":"mcpToolCall","output":"private"}),
        "turn"
    )
    .is_none());
    assert_eq!(
        protocol::split_session(&format!("claude:{}", Uuid::new_v4()))
            .unwrap()
            .0,
        Agent::Claude
    );
    assert!(protocol::split_session("unknown:session").is_err());
}

#[tokio::test]
async fn codex_controls_a_shared_session_with_scoped_approvals_and_live_text() {
    let directory = Directory::new();
    let socket_name = format!(".remote-{}.sock", Uuid::new_v4().simple());
    let socket_path = PathBuf::from(&socket_name);
    let listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
    struct SocketFile(PathBuf);
    impl Drop for SocketFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _file = SocketFile(socket_path.clone());
    let thread = Uuid::new_v4();
    let approvals = Arc::new(AtomicUsize::new(0));
    let accepted = approvals.clone();
    let mac = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Frame::Text(text))) = socket.next().await {
            let request: Value = serde_json::from_str(&text).unwrap();
            if request.get("method").is_none() {
                assert_eq!(request["result"]["decision"], "accept");
                accepted.fetch_add(1, Ordering::SeqCst);
                continue;
            }
            let result = match request["method"].as_str().unwrap() {
                "initialized" => continue,
                "initialize" => json!({"userAgent":"fixture"}),
                "thread/read" | "thread/resume" => {
                    json!({"thread":{"id":thread,"cwd":directory.0.to_str().unwrap(),"preview":"Fixture session","modelProvider":"litellm","updatedAt":1,"status":{"type":"idle"}}})
                }
                "config/read" => {
                    json!({"config":{"model_providers":{"litellm":{"base_url":"https://gateway.example.test/v1"}}}})
                }
                "turn/start" => {
                    assert_eq!(request["params"]["model"], "fixture-model");
                    assert_eq!(request["params"]["input"][0]["text"], "Continue");
                    socket.send(Frame::Text(json!({"method":"item/agentMessage/delta","params":{"threadId":thread,"turnId":"turn","itemId":"answer","delta":"Live answer"}}).to_string().into())).await.unwrap();
                    socket.send(Frame::Text(json!({"id":"permission-request","method":"item/commandExecution/requestApproval","params":{"threadId":thread,"turnId":"turn","itemId":"command","command":"pwd","availableDecisions":["accept","decline"]}}).to_string().into())).await.unwrap();
                    json!({"turn":{"id":"turn"}})
                }
                "thread/turns/list" => {
                    json!({"data":[{"id":"turn","status":"inProgress","items":[{"type":"userMessage","id":"input","content":[{"type":"text","text":"Continue"}]}]}],"nextCursor":null})
                }
                "turn/interrupt" => json!({}),
                other => panic!("unexpected RPC {other}"),
            };
            socket
                .send(Frame::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        }
    });
    let codex = codex::Codex::new(
        socket_path,
        "litellm".into(),
        Url::parse("https://gateway.example.test").unwrap(),
    );
    assert_eq!(
        codex
            .send(thread, "Continue".into(), Some("fixture-model".into()))
            .await
            .unwrap()["turnId"],
        "turn"
    );
    let detail = codex.read(thread).await.unwrap();
    assert!(detail
        .messages
        .iter()
        .any(|message| message.text == "Live answer"));
    assert_eq!(detail.approvals.len(), 1);
    let approval = Uuid::parse_str(&detail.approvals[0].id).unwrap();
    assert!(detail.approvals[0].can_accept);
    assert!(codex
        .approve(Uuid::new_v4(), approval, Decision::Accept)
        .await
        .is_err());
    codex
        .approve(thread, approval, Decision::Accept)
        .await
        .unwrap();
    assert!(codex
        .approve(thread, approval, Decision::Accept)
        .await
        .is_err());
    assert_eq!(codex.interrupt(thread).await.unwrap()["stopped"], true);
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
    mac.abort();
}

#[tokio::test]
async fn codex_new_session_remains_readable_before_its_first_turn_is_saved() {
    let directory = Directory::new();
    let path = PathBuf::from(format!(".remote-{}.sock", Uuid::new_v4().simple()));
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    struct SocketFile(PathBuf);
    impl Drop for SocketFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _file = SocketFile(path.clone());
    let cwd = directory.0.to_str().unwrap().to_owned();
    let project = cwd.clone();
    let thread = Uuid::new_v4();
    let mac = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        let mut reads = 0;
        while let Some(Ok(Frame::Text(text))) = socket.next().await {
            let request: Value = serde_json::from_str(&text).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "initialized" => continue,
                "initialize" => json!({"userAgent":"fixture"}),
                "config/read" => {
                    json!({"config":{"model_providers":{"litellm":{"base_url":"https://gateway.example.test/v1"}}}})
                }
                "thread/start" | "thread/read" => {
                    json!({"thread":{"id":thread,"cwd":project,"preview":"New session","modelProvider":"litellm","updatedAt":1,"status":{"type":"idle"}}})
                }
                "turn/start" => json!({"turn":{"id":"first-turn"}}),
                "thread/turns/list" => {
                    reads += 1;
                    if reads == 1 {
                        socket.send(Frame::Text(json!({"id":request["id"],"error":{"code":-1,"message":"Thread not persisted yet"}}).to_string().into())).await.unwrap();
                        continue;
                    }
                    json!({"data":[{"id":"first-turn","status":"completed","items":[{"type":"agentMessage","id":"answer","text":"Saved reply"}]}],"nextCursor":null})
                }
                other => panic!("unexpected RPC {other}"),
            };
            socket
                .send(Frame::Text(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
        }
    });
    let codex = codex::Codex::new(
        path,
        "litellm".into(),
        Url::parse("https://gateway.example.test").unwrap(),
    );
    let created = codex.create(cwd).await.unwrap();
    assert_eq!(created.id, format!("codex:{thread}"));
    assert!(codex.read(thread).await.unwrap().messages.is_empty());
    codex
        .send(thread, "First message".into(), None)
        .await
        .unwrap();
    let pending = codex.read(thread).await.unwrap();
    assert_eq!(pending.messages[0].text, "First message");
    assert!(pending.session.can_stop);
    assert_eq!(
        codex.read(thread).await.unwrap().messages[0].text,
        "Saved reply"
    );
    assert_eq!(codex.read(thread).await.unwrap().session.status, "ready");
    mac.abort();
}

#[test]
fn invalid_outcomes_and_endpoint_credentials_are_rejected() {
    let bad = Outcome {
        ok: true,
        value: None,
        status: None,
        error: None,
    };
    assert_eq!(bad.into_result().unwrap_err().status, 502);
    assert!(super::address(
        Some(Url::parse("http://remote.example.test").unwrap()),
        "UNUSED"
    )
    .is_err());
    assert!(super::address(
        Some(Url::parse("https://key:secret@remote.example.test").unwrap()),
        "UNUSED"
    )
    .is_err());
    assert!(super::address(
        Some(Url::parse("https://remote.example.test/?token=secret").unwrap()),
        "UNUSED"
    )
    .is_err());
    assert!(super::address(
        Some(Url::parse("https://remote.example.test").unwrap()),
        "UNUSED"
    )
    .is_ok());
}

#[tokio::test]
async fn claude_native_protocol_handoff_approvals_stop_and_route_changes() {
    use std::os::unix::fs::PermissionsExt;
    let directory = Directory::new();
    let endpoint = Arc::new(std::sync::Mutex::new(
        "https://gateway.example.test".to_string(),
    ));
    let selected = endpoint.clone();
    let relay = serve(Router::new().route("/api/status", get(move || { let selected = selected.clone(); async move {
        let gateway = selected.lock().unwrap().clone();
        Json(json!({"runtime":"rust","gateway_url":gateway,"broker":{"gateway_url":gateway}}))
    } }))).await;
    let home = directory.0.join("config");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("settings.json"),
        json!({"env":{"ANTHROPIC_BASE_URL":relay.url},"apiKeyHelper":"relay credential --proxy"})
            .to_string(),
    )
    .unwrap();
    let script = directory.0.join("claude-fixture");
    let cwd = directory.0.to_str().unwrap();
    let fixture = r#"#!/usr/bin/python3
import json, os, pathlib, sys, time
if sys.argv[1:] == ['agents', '--json']:
    active = os.path.join(os.environ['CLAUDE_CONFIG_DIR'], 'active.json')
    rows = json.loads(open(active).read()) if os.path.exists(active) else []
    live = []
    for row in rows:
        try:
            if row.get('fixture'):
                os.kill(row['pid'], 0)
            live.append(row)
        except ProcessLookupError:
            pass
    print(json.dumps(live))
    sys.exit(0)
assert not any(os.environ.get(key) for key in ['LITELLM_API_KEY', 'ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN'])
session = next(arg.split('=', 1)[1] for arg in sys.argv if arg.startswith(('--session-id=', '--resume=')))
home = pathlib.Path(os.environ['CLAUDE_CONFIG_DIR'])
if (home / 'pause-startup').exists():
    (home / 'active.json').write_text(json.dumps([{'sessionId':session,'pid':os.getpid(),'cwd':os.getcwd(),'fixture':True}]))
    (home / 'starting').touch()
def emit(value):
    print(json.dumps(value), flush=True)
for line in sys.stdin:
    value = json.loads(line)
    if value['type'] == 'control_request':
        while value['request']['subtype'] == 'initialize' and (home / 'pause-startup').exists():
            time.sleep(0.01)
        emit({'type':'control_response','response':{'subtype':'success','request_id':value['request_id'],'response':{}}})
        if value['request']['subtype'] == 'interrupt':
            if (home / 'reap-exit').exists():
                (home / 'exit-pid.tmp').write_text(str(os.getpid()))
                (home / 'exit-pid.tmp').replace(home / 'exit-pid')
                while (home / 'reap-exit').exists():
                    time.sleep(0.01)
                sys.exit(0)
            emit({'type':'result','subtype':'success','session_id':session})
    elif value['type'] == 'user':
        with open(os.path.join(os.environ['CLAUDE_CONFIG_DIR'], 'deliveries.jsonl'), 'a') as log:
            log.write(json.dumps({'pid':os.getpid(),'text':value['message']['content']}) + '\n')
        reported = '00000000-0000-4000-8000-000000000001' if value['message']['content'] == 'Mismatch' else session
        emit({'type':'system','subtype':'init','session_id':reported,'model':'fixture-model'})
        emit({'type':'stream_event','event':{'type':'message_start','message':{'id':'reply'}}})
        emit({'type':'assistant','uuid':'thinking','message':{'id':'reply','content':[{'type':'thinking','thinking':'Hidden'}]}})
        emit({'type':'stream_event','event':{'type':'content_block_delta','index':1,'delta':{'type':'text_delta','text':'Live Claude reply'}}})
        emit({'type':'control_request','request_id':'permission','request':{'subtype':'can_use_tool','tool_name':'Edit','input':{'file_path':'fixture.txt','old_string':'before','new_string':'after'}}})
    elif value['type'] == 'control_response':
        assert value['response']['response']['behavior'] == 'allow'
        assert value['response']['response']['updatedInput']['new_string'] == 'after'
        reply = {'type':'assistant','uuid':'uuid','cwd':os.getcwd(),'message':{'id':'reply','content':[{'type':'text','text':'Live Claude reply completed'}]}}
        project = home / 'projects' / 'fixture'
        project.mkdir(parents=True, exist_ok=True)
        with (project / (session + '.jsonl')).open('a') as transcript:
            transcript.write(json.dumps(reply) + '\n')
        emit(reply)
"#;
    std::fs::write(&script, fixture).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let claude = Arc::new(claude::Claude::new(
        script.clone(),
        home.clone(),
        directory.0.join("grants"),
        Url::parse("https://gateway.example.test").unwrap(),
    ));
    let session = claude.create(cwd.into()).unwrap();
    let (_, id) = protocol::split_session(&session.id).unwrap();
    assert!(claude
        .send(Uuid::new_v4(), "No consent".into(), None)
        .await
        .is_err());
    claude
        .send(id, "Continue".into(), Some("fixture-model".into()))
        .await
        .unwrap();
    let mut detail = claude.read(id).await.unwrap();
    for _ in 0..30 {
        if !detail.approvals.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        detail = claude.read(id).await.unwrap();
    }
    assert!(detail
        .messages
        .iter()
        .any(|message| message.text == "Live Claude reply"));
    assert_eq!(detail.session.status, "needs_approval");
    assert_eq!(
        detail
            .messages
            .iter()
            .filter(|message| message.role == "assistant")
            .count(),
        1
    );
    let approval = Uuid::parse_str(&detail.approvals[0].id).unwrap();
    assert!(claude.send(id, "Duplicate".into(), None).await.is_err());
    assert!(claude
        .approve(Uuid::new_v4(), approval, Decision::Accept)
        .await
        .is_err());
    *endpoint.lock().unwrap() = "https://different.example.test".into();
    assert_eq!(
        claude
            .approve(id, approval, Decision::Accept)
            .await
            .unwrap_err()
            .status,
        409
    );
    *endpoint.lock().unwrap() = "https://gateway.example.test".into();
    claude
        .approve(id, approval, Decision::Accept)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !claude
            .read(id)
            .await
            .unwrap()
            .messages
            .iter()
            .any(|message| message.text == "Live Claude reply completed")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let completed = claude.read(id).await.unwrap();
    assert_eq!(
        completed
            .messages
            .iter()
            .filter(|message| message.role == "assistant")
            .count(),
        1
    );
    let restored = claude::Claude::new(
        script.clone(),
        home.clone(),
        directory.0.join("grants"),
        Url::parse("https://gateway.example.test").unwrap(),
    );
    assert_eq!(
        restored
            .read(id)
            .await
            .unwrap()
            .messages
            .iter()
            .filter(|message| message.role == "assistant")
            .count(),
        1
    );
    assert!(claude
        .approve(id, approval, Decision::Accept)
        .await
        .is_err());
    assert_eq!(claude.interrupt(id).await.unwrap()["stopped"], true);
    let project = home.join("projects/fixture");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join(format!("{id}.jsonl")),
        format!(
            "{}\n",
            json!({"type":"user","cwd":cwd,"uuid":"first-context","message":{"content":"Continue"}})
        ),
    )
    .unwrap();
    claude.handoff(id).await.unwrap();
    let reopened = claude::Claude::new(
        script.clone(),
        home.clone(),
        directory.0.join("grants"),
        Url::parse("https://gateway.example.test").unwrap(),
    );
    assert_eq!(
        reopened.read(id).await.unwrap().session.model.as_deref(),
        Some("fixture-model")
    );
    let existing = Uuid::new_v4();
    std::fs::write(project.join(format!("{existing}.jsonl")), format!("{}\n{}\n{}\n",
        json!({"type":"user","cwd":cwd,"uuid":"original","message":{"content":"Original context"}}),
        json!({"type":"assistant","uuid":"chunk-a","message":{"id":"multi-block","content":[{"type":"text","text":"Repeated on purpose"}]}}),
        json!({"type":"assistant","uuid":"chunk-b","message":{"id":"multi-block","content":[{"type":"text","text":"Repeated on purpose"}]}}),
    )).unwrap();
    std::fs::write(
        home.join("active.json"),
        json!([{"sessionId":existing,"pid":1,"cwd":cwd}]).to_string(),
    )
    .unwrap();
    assert_eq!(claude.handoff(existing).await.unwrap_err().status, 409);
    std::fs::write(home.join("active.json"), "[]").unwrap();
    claude.handoff(existing).await.unwrap();
    assert_eq!(
        claude.read(existing).await.unwrap().messages[0].text,
        "Original context"
    );
    let repeated = claude.read(existing).await.unwrap();
    assert_eq!(repeated.messages.len(), 3);
    assert_eq!(repeated.messages[1].text, repeated.messages[2].text);
    assert_ne!(repeated.messages[1].id, repeated.messages[2].id);
    claude
        .send(existing, "Resume original".into(), None)
        .await
        .unwrap();
    let mismatch = claude.create(cwd.into()).unwrap();
    let (_, mismatch_id) = protocol::split_session(&mismatch.id).unwrap();
    claude
        .send(mismatch_id, "Mismatch".into(), None)
        .await
        .unwrap();
    let mut failed = claude.read(mismatch_id).await.unwrap();
    for _ in 0..30 {
        if failed.error.is_some() && !failed.session.can_stop {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        failed = claude.read(mismatch_id).await.unwrap();
    }
    assert!(failed.error.unwrap().contains("different session"));
    claude
        .send(mismatch_id, "After mismatch".into(), None)
        .await
        .unwrap();
    let mut deliveries = Vec::<Value>::new();
    for _ in 0..30 {
        deliveries = std::fs::read_to_string(home.join("deliveries.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        if deliveries
            .iter()
            .any(|delivery| delivery["text"] == "After mismatch")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let wrong_process = &deliveries
        .iter()
        .find(|delivery| delivery["text"] == "Mismatch")
        .unwrap()["pid"];
    assert_ne!(
        &deliveries
            .iter()
            .find(|delivery| delivery["text"] == "After mismatch")
            .unwrap()["pid"],
        wrong_process
    );
    let startup = claude.create(cwd.into()).unwrap();
    let (_, startup_id) = protocol::split_session(&startup.id).unwrap();
    std::fs::write(project.join(format!("{startup_id}.jsonl")), format!("{}\n", json!({"type":"user","cwd":cwd,"uuid":"startup-context","message":{"content":"Earlier context"}}))).unwrap();
    std::fs::write(home.join("pause-startup"), "").unwrap();
    let shared = claude.clone();
    let sending = tokio::spawn(async move {
        shared
            .send(startup_id, "Continue after startup".into(), None)
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !home.join("starting").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let starting = claude.read(startup_id).await.unwrap();
    assert_eq!(starting.session.status, "working");
    assert!(starting.session.can_stop);
    assert!(!starting.session.can_send);
    assert_eq!(starting.messages[0].text, "Earlier context");
    assert_eq!(claude.interrupt(startup_id).await.unwrap()["stopped"], true);
    assert!(sending.await.unwrap().is_err());
    std::fs::remove_file(home.join("pause-startup")).unwrap();
    claude
        .send(startup_id, "Retry after stop".into(), None)
        .await
        .unwrap();
    std::fs::write(home.join("reap-exit"), "").unwrap();
    claude.interrupt(startup_id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !home.join("exit-pid").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let pid = std::fs::read_to_string(home.join("exit-pid"))
        .unwrap()
        .parse::<libc::pid_t>()
        .unwrap();
    std::fs::remove_file(home.join("reap-exit")).unwrap();
    assert_eq!(unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) }, pid);
    tokio::time::timeout(Duration::from_secs(5), async {
        while claude.read(startup_id).await.unwrap().session.can_stop {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    claude
        .send(startup_id, "Retry after process exit".into(), None)
        .await
        .unwrap();
}
