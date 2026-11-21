use std::{
    os::fd::AsFd,
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

static CLAUDE_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

struct ShortSocket(PathBuf);

impl ShortSocket {
    fn listen() -> (Self, tokio::net::UnixListener) {
        let id = Uuid::new_v4().simple().to_string();
        let path = PathBuf::from(format!("/tmp/r{}.sock", &id[..8]));
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        (Self(path), listener)
    }
}

impl Drop for ShortSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
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
        let issued = store
            .issue("owner", &["fixture-model".into()], 100)
            .unwrap();
        let token = issued.access_token;
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
        (token, device_token, device.id, issued.identity.expires_at)
    };
    let store = Store::open(&path).unwrap();
    assert_eq!(store.models(&token).unwrap(), vec!["fixture-model"]);
    assert_eq!(
        store
            .identity(&token, expires - 1)
            .unwrap()
            .unwrap()
            .subject,
        "owner"
    );
    assert!(store.identity(&token, expires).unwrap().is_none());
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

#[test]
fn short_lived_credentials_expire_rotate_and_revoke_without_sleeping() {
    let directory = Directory::new();
    let store = Store::open(&directory.0.join("control.sqlite")).unwrap();
    let issued = store
        .issue("owner", &["fixture-model".into()], 1_000)
        .unwrap();
    assert_eq!(
        issued.identity.expires_at,
        1_000 + super::store::ACCESS_TTL_MS
    );
    assert_eq!(
        store.models(&issued.access_token).unwrap(),
        vec!["fixture-model"]
    );
    let saved = directory
        .0
        .read_dir()
        .unwrap()
        .filter_map(|entry| std::fs::read(entry.ok()?.path()).ok())
        .fold(String::new(), |mut saved, bytes| {
            saved.push_str(&String::from_utf8_lossy(&bytes));
            saved
        });
    assert!(!saved.contains(&issued.access_token));
    assert!(!saved.contains(&issued.refresh_token));
    assert!(store
        .identity(&issued.access_token, issued.identity.expires_at - 1)
        .unwrap()
        .is_some());
    assert!(store
        .identity(&issued.access_token, issued.identity.expires_at)
        .unwrap()
        .is_none());

    let first = store.issue("owner", &["fixture-model".into()], 0).unwrap();
    let rotated = store.refresh(&first.refresh_token, 1_000).unwrap();
    assert_ne!(rotated.refresh_token, first.refresh_token);
    assert_ne!(rotated.access_token, first.access_token);
    assert_eq!(
        store.models(&rotated.access_token).unwrap(),
        vec!["fixture-model"]
    );
    assert!(store.refresh(&first.refresh_token, 2_000).is_err());
    assert!(store
        .identity(&rotated.access_token, 2_000)
        .unwrap()
        .is_none());
    assert!(store.refresh(&rotated.refresh_token, 2_000).is_err());

    let idle = store.issue("owner", &["fixture-model".into()], 0).unwrap();
    assert!(store
        .refresh(&idle.refresh_token, super::store::REFRESH_IDLE_MS)
        .is_err());
    let absolute = store.issue("owner", &["fixture-model".into()], 0).unwrap();
    let moved = store.refresh(&absolute.refresh_token, 1_000).unwrap();
    assert!(store
        .refresh(&moved.refresh_token, super::store::REFRESH_ABSOLUTE_MS)
        .is_err());

    let legacy = protocol::token();
    {
        let db = rusqlite::Connection::open(directory.0.join("control.sqlite")).unwrap();
        db.execute(
            "INSERT INTO sessions VALUES (?, 'legacy', 'legacy', ?)",
            rusqlite::params![protocol::digest(&legacy), 5_000 + 7 * 86_400_000],
        )
        .unwrap();
        db.execute(
            "INSERT INTO catalogs VALUES (?, ?)",
            rusqlite::params![protocol::digest(&legacy), "[\"legacy-model\"]"],
        )
        .unwrap();
    }
    let reopened = Store::open(&directory.0.join("control.sqlite")).unwrap();
    assert_eq!(
        reopened.identity(&legacy, 5_000).unwrap().unwrap().subject,
        "legacy"
    );
    assert_eq!(
        reopened.models(&legacy).unwrap(),
        vec!["legacy-model".to_string()]
    );

    let (device, device_token) = reopened.enroll("owner", "Mac", 1_000).unwrap();
    let (access, expires) = reopened.exchange_device(&device_token, 1_000).unwrap();
    assert_eq!(expires, 1_000 + super::store::ACCESS_TTL_MS);
    assert!(reopened
        .authenticate_access(&device_token, 1_000)
        .unwrap()
        .is_none());
    assert_eq!(
        reopened
            .authenticate_access(&access, expires - 1)
            .unwrap()
            .unwrap()
            .0
            .id,
        device.id
    );
    assert!(reopened
        .authenticate_access(&access, expires)
        .unwrap()
        .is_none());
    reopened.revoke(device.id, "owner").unwrap();
    assert!(reopened
        .authenticate_access(&access, 1_000)
        .unwrap()
        .is_none());
    assert!(reopened.exchange_device(&device_token, 1_000).is_err());
}

#[test]
fn connector_refreshes_before_expiry_and_after_rejection() {
    let mut access = super::connector::DeviceAccess::new("device-credential");
    assert!(access.needs_refresh(0));
    let ttl = 15 * 60 * 1000;
    access.remember("cached", 0, ttl);
    assert!(!access.needs_refresh(ttl * 4 / 5 - 1));
    assert!(access.needs_refresh(ttl * 4 / 5));
    access.reject();
    assert!(access.needs_refresh(0));
}

#[tokio::test]
async fn agent_socket_rejects_the_device_credential_and_expired_access_tokens() {
    let gateway = gateway().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let service = Service::open(
        gateway.url.clone(),
        url.clone(),
        std::path::Path::new(":memory:"),
    )
    .unwrap();
    service.set_clock(1_000_000);
    let served = service.clone();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            served
                .router()
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let server = Server {
        url: url.clone(),
        task,
    };
    let owner = sign_in(&server).await;
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
    let device_token = device["deviceToken"].as_str().unwrap();
    let mut endpoint = server.url.join("/agent").unwrap();
    endpoint.set_scheme("ws").unwrap();
    let connect = |bearer: &str| {
        let mut request = endpoint.as_str().into_client_request().unwrap();
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {bearer}").parse().unwrap());
        request
    };
    assert!(tokio_tungstenite::connect_async(connect(device_token))
        .await
        .is_err());
    let mut session = super::connector::DeviceAccess::new(device_token);
    let (access, expires) =
        super::connector::ensure_device_access(&server.url, &mut session, 1_000_000)
            .await
            .unwrap();
    assert!(tokio_tungstenite::connect_async(connect(&access))
        .await
        .is_ok());
    session.remember("stale-token", 1_000_000, expires);
    session.reject();
    let (renewed, _) = super::connector::ensure_device_access(&server.url, &mut session, 1_000_000)
        .await
        .unwrap();
    assert_ne!(renewed, "stale-token");
    assert_ne!(renewed, access);
    service.set_clock(expires);
    assert!(tokio_tungstenite::connect_async(connect(&access))
        .await
        .is_err());
    assert!(tokio_tungstenite::connect_async(connect(&renewed))
        .await
        .is_err());
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
    let device_access: Value = client()
        .post(server.url.join("/api/auth/device").unwrap())
        .bearer_auth(device["deviceToken"].as_str().unwrap())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
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
        format!("Bearer {}", device_access["token"].as_str().unwrap())
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
    for text in [
        "<command-name>/model</command-name>\n            <command-message>model</command-message>\n            <command-args>fixture-model</command-args>",
        "<local-command-stdout>Set model to fixture-model</local-command-stdout>",
    ] {
        for content in [json!(text), json!([{"type":"text","text":text}])] {
            assert!(claude::visible(&json!({"content":content}), "command", "user").is_empty());
            assert_eq!(
                claude::visible(&json!({"content":content}), "reply", "assistant")[0].text,
                text
            );
        }
    }
    for text in [
        "Please explain /model and how to select a gateway model.",
        "<command-name>/model</command-name>",
        "<local-command-stdout>example</local-command-stdout>\nPlease explain this snippet.",
    ] {
        assert_eq!(
            claude::visible(&json!({"content":text}), "prompt", "user")[0].text,
            text
        );
    }
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
async fn codex_rejects_a_socket_path_longer_than_sun_path() {
    let path = PathBuf::from(format!("/tmp/{}", "s".repeat(120)));
    let codex = codex::Codex::new(
        path,
        "litellm".into(),
        Url::parse("https://gateway.example.test").unwrap(),
    );
    let error = codex.read(Uuid::new_v4()).await.unwrap_err();
    assert!(
        error.error.contains("must be shorter than"),
        "{}",
        error.error
    );
    assert!(!error.error.contains("Keep Codex running"));
}

#[tokio::test]
async fn codex_replays_a_pending_approval_after_the_connector_restarts() {
    let (socket, listener) = ShortSocket::listen();
    let socket_path = socket.0.clone();
    let thread = Uuid::new_v4();
    let resumes = Arc::new(AtomicUsize::new(0));
    let observed_resumes = resumes.clone();
    let listener = Arc::new(listener);
    for decision in [Decision::Accept, Decision::Decline] {
        resumes.store(0, Ordering::SeqCst);
        codex_replays_a_pending_approval(
            socket_path.clone(),
            listener.clone(),
            thread,
            observed_resumes.clone(),
            decision,
            true,
        )
        .await;
    }
}

#[tokio::test]
async fn codex_accepts_before_detail_after_restart() {
    codex_answers_before_detail(Decision::Accept).await;
}

#[tokio::test]
async fn codex_declines_before_detail_after_restart() {
    codex_answers_before_detail(Decision::Decline).await;
}

async fn codex_answers_before_detail(decision: Decision) {
    let (socket, listener) = ShortSocket::listen();
    codex_replays_a_pending_approval(
        socket.0.clone(),
        Arc::new(listener),
        Uuid::new_v4(),
        Arc::new(AtomicUsize::new(0)),
        decision,
        false,
    )
    .await;
}

async fn codex_replays_a_pending_approval(
    socket_path: PathBuf,
    listener: Arc<tokio::net::UnixListener>,
    thread: Uuid,
    observed_resumes: Arc<AtomicUsize>,
    decision: Decision,
    read_before_answer: bool,
) {
    let resumes = observed_resumes.clone();
    let decisions = Arc::new(AtomicUsize::new(0));
    let observed_decisions = decisions.clone();
    let executions = Arc::new(AtomicUsize::new(0));
    let observed_executions = executions.clone();
    let server = tokio::spawn(async move {
        let mut pending = false;
        let mut completed = false;
        for _ in 0..3 {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(Frame::Text(text))) = socket.next().await {
                let request: Value = serde_json::from_str(&text).unwrap();
                if request.get("method").is_none() {
                    assert_eq!(request["id"], 17);
                    assert_eq!(
                        request["result"]["decision"],
                        serde_json::to_value(decision).unwrap()
                    );
                    assert!(pending, "The same approval was answered twice");
                    observed_decisions.fetch_add(1, Ordering::SeqCst);
                    if decision == Decision::Accept {
                        observed_executions.fetch_add(1, Ordering::SeqCst);
                    }
                    pending = false;
                    completed = true;
                    socket
                        .send(Frame::Text(
                            json!({"method":"serverRequest/resolved","params":{"requestId":17}})
                                .to_string()
                                .into(),
                        ))
                        .await
                        .unwrap();
                    continue;
                }
                let method = request["method"].as_str().unwrap();
                if method == "initialized" {
                    continue;
                }
                let metadata = json!({"id":thread,"cwd":"/repo","preview":"Approval reconnect","modelProvider":"litellm","updatedAt":1,"status":if pending {json!({"type":"active","activeFlags":["waitingOnApproval"]})} else {json!({"type":"idle"})}});
                let result = match method {
                    "initialize" => json!({}),
                    "thread/read" => json!({"thread":metadata}),
                    "config/read" => {
                        json!({"config":{"model_providers":{"litellm":{"base_url":"https://gateway.example.test"}}}})
                    }
                    "thread/resume" => {
                        observed_resumes.fetch_add(1, Ordering::SeqCst);
                        json!({"thread":metadata,"approvalPolicy":"on-request"})
                    }
                    "turn/start" => {
                        pending = true;
                        json!({"turn":{"id":"turn"}})
                    }
                    "thread/turns/list" => {
                        json!({"data":[{"id":"turn","status":if completed {"completed"} else {"inProgress"},"items":[],"error":null}]})
                    }
                    _ => panic!("Unexpected method {method}"),
                };
                socket
                    .send(Frame::Text(
                        json!({"id":request["id"],"result":result})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
                if pending && matches!(method, "turn/start" | "thread/resume") {
                    let approval = Frame::Text(json!({"id":17,"method":"item/commandExecution/requestApproval","params":{"threadId":thread,"turnId":"turn","itemId":"command","command":"printf reconnect-ok","availableDecisions":["accept","decline"]}}).to_string().into());
                    socket.send(approval.clone()).await.unwrap();
                    socket.send(approval).await.unwrap();
                }
            }
        }
    });
    let gateway = Url::parse("https://gateway.example.test").unwrap();
    let original = codex::Codex::new(socket_path.clone(), "litellm".into(), gateway.clone());
    original
        .send(thread, "Run the command".into(), None)
        .await
        .unwrap();
    let before = original.read(thread).await.unwrap();
    assert_eq!(before.approvals.len(), 1);
    let old_id = Uuid::parse_str(&before.approvals[0].id).unwrap();
    drop(original);

    let restarted = codex::Codex::new(socket_path.clone(), "litellm".into(), gateway.clone());
    if read_before_answer {
        let restored = restarted.read(thread).await.unwrap();
        assert_eq!(restored.session.status, "needs_approval");
        assert_eq!(
            restored.approvals.len(),
            1,
            "The daemon still holds an answerable approval"
        );
        assert!(restored.approvals[0].can_accept);
        assert_eq!(
            restored.approvals[0].id,
            old_id.to_string(),
            "Public approval ID changed after connector restart"
        );
        assert_eq!(restarted.read(thread).await.unwrap().approvals.len(), 1);
    } else {
        assert_eq!(resumes.load(Ordering::SeqCst), 1);
    }
    restarted.approve(thread, old_id, decision).await.unwrap();
    let resolved = restarted.read(thread).await.unwrap();
    assert!(resolved.approvals.is_empty());
    assert_eq!(resolved.session.status, "ready");
    assert!(!resolved.session.can_stop);
    assert_eq!(
        resumes.load(Ordering::SeqCst),
        2,
        "Attach once per connection, not on every poll or answer"
    );
    let repeated = restarted
        .approve(thread, old_id, decision)
        .await
        .unwrap_err();
    assert_eq!(repeated.status, 409);
    assert_eq!(resumes.load(Ordering::SeqCst), 2);
    drop(restarted);
    let after_completion = codex::Codex::new(socket_path, "litellm".into(), gateway);
    let repeated = after_completion
        .approve(thread, old_id, decision)
        .await
        .unwrap_err();
    assert_eq!(repeated.status, 409);
    assert_eq!(decisions.load(Ordering::SeqCst), 1);
    assert_eq!(
        executions.load(Ordering::SeqCst),
        usize::from(decision == Decision::Accept)
    );
    server.abort();
}

#[derive(Clone, Copy, PartialEq)]
enum ApprovalFault {
    Resume,
    HistoryBeforeReplay,
    HistoryAfterReplay,
    ClosedHistory,
    ReplacedSocket,
    SocketWrite,
}

#[tokio::test]
async fn codex_approval_retries_after_resume_failure() {
    codex_approval_fault(ApprovalFault::Resume).await;
}

#[tokio::test]
async fn codex_approval_retries_after_history_socket_closes() {
    codex_approval_fault(ApprovalFault::ClosedHistory).await;
}

#[tokio::test]
async fn codex_approval_retries_after_history_failure_before_replay() {
    codex_approval_fault(ApprovalFault::HistoryBeforeReplay).await;
}

#[tokio::test]
async fn codex_approval_retries_after_history_failure_with_replay() {
    codex_approval_fault(ApprovalFault::HistoryAfterReplay).await;
}

#[tokio::test]
async fn codex_approval_uses_the_socket_that_restored_it() {
    codex_approval_fault(ApprovalFault::ReplacedSocket).await;
}

#[tokio::test]
async fn codex_approval_retries_after_a_failed_socket_write() {
    codex_approval_fault(ApprovalFault::SocketWrite).await;
}

async fn codex_approval_fault(fault: ApprovalFault) {
    for decision in [Decision::Accept, Decision::Decline] {
        let (path, listener) = ShortSocket::listen();
        let thread = Uuid::new_v4();
        let decisions = Arc::new(AtomicUsize::new(0));
        let executions = Arc::new(AtomicUsize::new(0));
        let answered_on = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let observed_decisions = decisions.clone();
        let observed_executions = executions.clone();
        let observed_connection = answered_on.clone();
        let release_socket = release.clone();
        let server = tokio::spawn(async move {
            let mut pending = false;
            let mut failed = false;
            for connection in 0..3 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                while let Some(Ok(Frame::Text(text))) = socket.next().await {
                    let request: Value = serde_json::from_str(&text).unwrap();
                    let Some(method) = request["method"].as_str() else {
                        assert_eq!(request["id"], 17);
                        assert_eq!(
                            request["result"]["decision"],
                            serde_json::to_value(decision).unwrap()
                        );
                        assert!(pending, "The command must not be answered twice");
                        pending = false;
                        observed_decisions.fetch_add(1, Ordering::SeqCst);
                        observed_executions
                            .fetch_add(usize::from(decision == Decision::Accept), Ordering::SeqCst);
                        observed_connection.store(connection, Ordering::SeqCst);
                        socket.send(Frame::Text(json!({"method":"serverRequest/resolved","params":{"requestId":17}}).to_string().into())).await.unwrap();
                        continue;
                    };
                    if method == "initialized" {
                        continue;
                    }
                    let metadata = json!({"id":thread,"cwd":"/repo","preview":"Retry approval","modelProvider":"litellm","updatedAt":1,"status":if pending {json!({"type":"active","activeFlags":["waitingOnApproval"]})} else {json!({"type":"idle"})}});
                    if connection == 1
                        && !failed
                        && ((method == "thread/turns/list"
                            && matches!(
                                fault,
                                ApprovalFault::HistoryBeforeReplay
                                    | ApprovalFault::HistoryAfterReplay
                            ))
                            || (method == "thread/resume" && fault == ApprovalFault::Resume))
                    {
                        failed = true;
                        socket.send(Frame::Text(json!({"id":request["id"],"error":{"message":"History temporarily unavailable"}}).to_string().into())).await.unwrap();
                        continue;
                    }
                    let result = match method {
                        "initialize" => json!({}),
                        "thread/read" | "thread/resume" => {
                            json!({"thread":metadata,"approvalPolicy":"on-request"})
                        }
                        "config/read" => {
                            json!({"config":{"model_providers":{"litellm":{"base_url":"https://gateway.example.test"}}}})
                        }
                        "turn/start" => {
                            pending = true;
                            json!({"turn":{"id":"turn"}})
                        }
                        "thread/turns/list" => {
                            json!({"data":[{"id":"turn","status":if pending {"inProgress"} else {"completed"},"items":[]}]})
                        }
                        _ => panic!("Unexpected RPC {method}"),
                    };
                    let response = Frame::Text(
                        json!({"id":request["id"],"result":result})
                            .to_string()
                            .into(),
                    );
                    if connection == 1
                        && ((fault == ApprovalFault::ReplacedSocket
                            && ((decision == Decision::Accept && method == "config/read")
                                || (decision == Decision::Decline && method == "thread/read")))
                            || (fault == ApprovalFault::ClosedHistory
                                && method == "thread/turns/list"))
                    {
                        socket.feed(response).await.unwrap();
                        socket.feed(Frame::Close(None)).await.unwrap();
                        socket.flush().await.unwrap();
                        break;
                    }
                    let fail_write = connection == 1
                        && !failed
                        && method == "thread/turns/list"
                        && fault == ApprovalFault::SocketWrite;
                    if fail_write {
                        failed = true;
                        let fd = socket.get_ref().as_fd().try_clone_to_owned().unwrap();
                        std::os::unix::net::UnixStream::from(fd)
                            .shutdown(std::net::Shutdown::Read)
                            .unwrap();
                    }
                    socket.send(response).await.unwrap();
                    if fail_write {
                        release_socket.notified().await;
                        break;
                    }
                    if pending
                        && matches!(method, "turn/start" | "thread/resume")
                        && !(connection == 1
                            && !failed
                            && fault == ApprovalFault::HistoryBeforeReplay)
                    {
                        socket.send(Frame::Text(json!({"id":17,"method":"item/commandExecution/requestApproval","params":{"threadId":thread,"turnId":"turn","itemId":"command","command":"echo retry","availableDecisions":["accept","decline"]}}).to_string().into())).await.unwrap();
                    }
                }
            }
        });
        let gateway = Url::parse("https://gateway.example.test").unwrap();
        let original = codex::Codex::new(path.0.clone(), "litellm".into(), gateway.clone());
        original
            .send(thread, "Run the command".into(), None)
            .await
            .unwrap();
        let old_id =
            Uuid::parse_str(&original.read(thread).await.unwrap().approvals[0].id).unwrap();
        drop(original);
        let restarted = codex::Codex::new(path.0.clone(), "litellm".into(), gateway);
        let first = restarted.approve(thread, old_id, decision).await;
        release.notify_one();
        if fault == ApprovalFault::ReplacedSocket {
            first.expect("The approval must be answered on the restored socket");
        } else {
            assert_eq!(
                first.unwrap_err().status,
                if matches!(
                    fault,
                    ApprovalFault::SocketWrite | ApprovalFault::ClosedHistory
                ) {
                    503
                } else {
                    502
                }
            );
            restarted
                .approve(thread, old_id, decision)
                .await
                .expect("The pending approval must remain answerable after a transient failure");
        }
        assert!(restarted.read(thread).await.unwrap().approvals.is_empty());
        assert_eq!(
            restarted
                .approve(thread, old_id, decision)
                .await
                .unwrap_err()
                .status,
            409
        );
        assert_eq!(decisions.load(Ordering::SeqCst), 1);
        assert_eq!(
            executions.load(Ordering::SeqCst),
            usize::from(decision == Decision::Accept)
        );
        assert_eq!(
            answered_on.load(Ordering::SeqCst),
            if matches!(
                fault,
                ApprovalFault::SocketWrite
                    | ApprovalFault::ReplacedSocket
                    | ApprovalFault::ClosedHistory
            ) {
                2
            } else {
                1
            }
        );
        server.abort();
    }
}

#[tokio::test]
async fn codex_controls_a_shared_session_with_scoped_approvals_and_live_text() {
    let directory = Directory::new();
    let (socket, listener) = ShortSocket::listen();
    let socket_path = socket.0.clone();
    let thread = Uuid::new_v4();
    let approvals = Arc::new(AtomicUsize::new(0));
    let accepted = approvals.clone();
    let denials = Arc::new(AtomicUsize::new(0));
    let declined = denials.clone();
    let constrained = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let enforced = constrained.clone();
    let thread_policy = Arc::new(std::sync::Mutex::new("never".to_string()));
    let saved_policy = thread_policy.clone();
    let requested_policy = Arc::new(std::sync::Mutex::new(None::<Option<String>>));
    let observed_policy = requested_policy.clone();
    let mac = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Frame::Text(text))) = socket.next().await {
            let request: Value = serde_json::from_str(&text).unwrap();
            if request.get("method").is_none() {
                match request["result"]["decision"].as_str().unwrap() {
                    "accept" => {
                        accepted.fetch_add(1, Ordering::SeqCst);
                    }
                    "decline" => {
                        declined.fetch_add(1, Ordering::SeqCst);
                    }
                    other => panic!("unexpected decision {other}"),
                }
                continue;
            }
            let result = match request["method"].as_str().unwrap() {
                "initialized" => continue,
                "initialize" => json!({"userAgent":"fixture"}),
                "thread/read" => {
                    json!({"thread":{"id":thread,"cwd":directory.0.to_str().unwrap(),"preview":"Fixture session","modelProvider":"litellm","updatedAt":1,"status":{"type":"idle"}}})
                }
                // Real thread/resume puts AskForApproval beside `thread`, never inside it.
                "thread/resume" => {
                    let policy = saved_policy.lock().unwrap().clone();
                    let policy = if policy == "granular" {
                        json!({"granular":{"sandbox_approval":true,"rules":true,"skill_approval":false,"request_permissions":false,"mcp_elicitations":false}})
                    } else {
                        json!(policy)
                    };
                    json!({"thread":{"id":thread,"cwd":directory.0.to_str().unwrap(),"preview":"Fixture session","modelProvider":"litellm","updatedAt":1,"status":{"type":"idle"}},"model":"fixture-model","modelProvider":"litellm","approvalPolicy":policy,"serviceTier":null,"sandbox":"workspace-write"})
                }
                "config/read" => {
                    json!({"config":{"model_providers":{"litellm":{"base_url":"https://gateway.example.test/v1"}}}})
                }
                "turn/start" => {
                    assert_eq!(request["params"]["model"], "fixture-model");
                    assert_eq!(request["params"]["input"][0]["text"], "Continue");
                    let requested = request["params"]["approvalPolicy"]
                        .as_str()
                        .map(str::to_owned);
                    *observed_policy.lock().unwrap() = Some(requested.clone());
                    let policy = requested.unwrap_or_else(|| saved_policy.lock().unwrap().clone());
                    if policy == "on-request" && enforced.load(Ordering::SeqCst) {
                        socket.send(Frame::Text(json!({"id":request["id"],"error":{"code":-32600,"message":"Approval policy is restricted by your administrator."}}).to_string().into())).await.unwrap();
                        continue;
                    }
                    if policy == "never" {
                        socket.send(Frame::Text(json!({"method":"item/agentMessage/delta","params":{"threadId":thread,"turnId":"turn","itemId":"answer","delta":"Permission was denied for the escalated command."}}).to_string().into())).await.unwrap();
                        socket
                            .send(Frame::Text(
                                json!({"id":request["id"],"result":{"turn":{"id":"turn"}}})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .unwrap();
                        continue;
                    }
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
    assert_eq!(
        requested_policy.lock().unwrap().clone(),
        Some(Some("on-request".into())),
        "a never-policy thread must request interactive approvals"
    );
    let detail = codex.read(thread).await.unwrap();
    assert_eq!(
        detail.approvals.len(),
        1,
        "terminal policy silently denied the tool without a card"
    );
    assert_eq!(approvals.load(Ordering::SeqCst), 0);
    assert_eq!(denials.load(Ordering::SeqCst), 0);
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
    codex
        .send(thread, "Continue".into(), Some("fixture-model".into()))
        .await
        .unwrap();
    let pending = codex.read(thread).await.unwrap();
    assert_eq!(pending.approvals.len(), 1);
    let approval = Uuid::parse_str(&pending.approvals[0].id).unwrap();
    codex
        .approve(thread, approval, Decision::Decline)
        .await
        .unwrap();
    assert_eq!(codex.interrupt(thread).await.unwrap()["stopped"], true);
    assert_eq!(denials.load(Ordering::SeqCst), 1);
    constrained.store(true, Ordering::SeqCst);
    let error = codex
        .send(thread, "Continue".into(), Some("fixture-model".into()))
        .await
        .unwrap_err();
    assert_eq!(error.status, 502);
    assert!(error.error.contains("restricted by your administrator"));
    assert!(codex.read(thread).await.unwrap().approvals.is_empty());
    *thread_policy.lock().unwrap() = "untrusted".into();
    codex
        .send(thread, "Continue".into(), Some("fixture-model".into()))
        .await
        .unwrap();
    assert_eq!(
        requested_policy.lock().unwrap().clone(),
        Some(None),
        "a stricter resumed policy must not be overridden"
    );
    let strict = codex.read(thread).await.unwrap();
    assert_eq!(strict.approvals.len(), 1);
    let approval = Uuid::parse_str(&strict.approvals[0].id).unwrap();
    codex
        .approve(thread, approval, Decision::Decline)
        .await
        .unwrap();
    assert_eq!(codex.interrupt(thread).await.unwrap()["stopped"], true);
    assert_eq!(denials.load(Ordering::SeqCst), 2);
    *thread_policy.lock().unwrap() = "granular".into();
    codex
        .send(thread, "Continue".into(), Some("fixture-model".into()))
        .await
        .expect("an object-form granular policy must still decode");
    assert_eq!(
        requested_policy.lock().unwrap().clone(),
        Some(None),
        "a granular object policy must not be overridden"
    );
    assert_eq!(codex.interrupt(thread).await.unwrap()["stopped"], true);
    mac.abort();
}

#[tokio::test]
async fn codex_stop_then_send_shows_the_new_turn_instead_of_interrupted_live_text() {
    let directory = Directory::new();
    let (socket, listener) = ShortSocket::listen();
    let path = socket.0.clone();
    let thread = Uuid::new_v4();
    let phase = Arc::new(AtomicUsize::new(0));
    let stage = phase.clone();
    let mac = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        while let Some(Ok(Frame::Text(text))) = socket.next().await {
            let request: Value = serde_json::from_str(&text).unwrap();
            if request.get("method").is_none() {
                continue;
            }
            let result = match request["method"].as_str().unwrap() {
                "initialized" => continue,
                "initialize" => json!({"userAgent":"fixture"}),
                "thread/read" => {
                    json!({"thread":{"id":thread,"cwd":directory.0.to_str().unwrap(),"preview":"Fixture session","modelProvider":"litellm","updatedAt":1,"status":{"type":"idle"}}})
                }
                "thread/resume" => {
                    json!({"thread":{"id":thread,"cwd":directory.0.to_str().unwrap(),"preview":"Fixture session","modelProvider":"litellm","updatedAt":1,"status":{"type":"idle"}},"approvalPolicy":"on-request","model":"fixture-model","modelProvider":"litellm"})
                }
                "config/read" => {
                    json!({"config":{"model_providers":{"litellm":{"base_url":"https://gateway.example.test/v1"}}}})
                }
                "turn/start" => {
                    let fresh = stage.load(Ordering::SeqCst) > 0;
                    let turn = if fresh { "new-turn" } else { "old-turn" };
                    let item = if fresh { "fresh" } else { "draft" };
                    let delta = if fresh {
                        "fresh output"
                    } else {
                        "stale draft output"
                    };
                    socket.send(Frame::Text(json!({"method":"item/agentMessage/delta","params":{"threadId":thread,"turnId":turn,"itemId":item,"delta":delta}}).to_string().into())).await.unwrap();
                    if fresh {
                        stage.store(2, Ordering::SeqCst);
                    }
                    json!({"turn":{"id":turn}})
                }
                "thread/turns/list" => {
                    let finalized = json!({"id":"old-turn","status":"completed","items":[
                        {"type":"userMessage","id":"input","content":[{"type":"text","text":"First"}]},
                        {"type":"agentMessage","id":"draft","text":"finalized partial"}
                    ]});
                    match stage.load(Ordering::SeqCst) {
                        3 => {
                            socket.send(Frame::Text(json!({"method":"item/completed","params":{"threadId":thread,"turnId":"old-turn","item":{"type":"agentMessage","id":"draft"}}}).to_string().into())).await.unwrap();
                            json!({"data":[{"id":"old-turn","status":"inProgress","items":[{"type":"agentMessage","id":"draft","text":"stale draft output"}]}],"nextCursor":null})
                        }
                        0 => {
                            json!({"data":[{"id":"old-turn","status":"inProgress","items":[{"type":"userMessage","id":"input","content":[{"type":"text","text":"First"}]}]}],"nextCursor":null})
                        }
                        1 => json!({"data":[finalized],"nextCursor":null}),
                        _ => json!({"data":[
                            {"id":"new-turn","status":"inProgress","items":[{"type":"userMessage","id":"input","content":[{"type":"text","text":"Second"}]}]},
                            finalized
                        ],"nextCursor":null}),
                    }
                }
                "turn/interrupt" => {
                    stage.store(1, Ordering::SeqCst);
                    socket.send(Frame::Text(json!({"method":"turn/completed","params":{"threadId":thread,"turn":{"id":"old-turn","status":"interrupted"}}}).to_string().into())).await.unwrap();
                    json!({})
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
    codex
        .send(thread, "First".into(), Some("fixture-model".into()))
        .await
        .unwrap();
    let streaming = codex.read(thread).await.unwrap();
    assert_eq!(
        streaming.streaming_reply_id.as_deref(),
        Some("old-turn/draft")
    );
    assert!(
        streaming
            .messages
            .iter()
            .any(|message| message.text == "stale draft output"),
        "the in-progress turn is visible while it streams"
    );
    phase.store(3, Ordering::SeqCst);
    let completed_item = codex.read(thread).await.unwrap();
    assert!(completed_item.streaming_reply_id.is_none());
    assert!(completed_item
        .messages
        .iter()
        .any(|message| message.text == "stale draft output"));
    assert_eq!(codex.interrupt(thread).await.unwrap()["stopped"], true);
    assert_eq!(phase.load(Ordering::SeqCst), 1);
    codex
        .send(thread, "Second".into(), Some("fixture-model".into()))
        .await
        .unwrap();
    let continued = codex.read(thread).await.unwrap();
    assert_eq!(continued.session.status, "ready");
    assert!(
        continued
            .messages
            .iter()
            .any(|message| message.text == "fresh output"),
        "the turn after Stop must show its own output"
    );
    assert!(
        continued
            .messages
            .iter()
            .any(|message| message.text == "finalized partial"),
        "the interrupted turn keeps the text Codex saved"
    );
    assert!(
        continued
            .messages
            .iter()
            .all(|message| message.text != "stale draft output"),
        "interrupted live text must not stay after Stop"
    );
    mac.abort();
}

#[tokio::test]
async fn codex_new_session_remains_readable_before_its_first_turn_is_saved() {
    let directory = Directory::new();
    let (socket, listener) = ShortSocket::listen();
    let path = socket.0.clone();
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
async fn claude_child_never_inherits_anthropic_credentials_without_a_litellm_key() {
    use std::os::unix::fs::PermissionsExt;
    let _env = CLAUDE_ENV.lock().await;
    let _restore = CredentialEnv::set(&[
        ("ANTHROPIC_API_KEY", Some("sk-inherited-anthropic")),
        ("ANTHROPIC_AUTH_TOKEN", Some("sk-inherited-token")),
        ("OPENAI_API_KEY", Some("sk-inherited-openai")),
        ("AWS_SECRET_ACCESS_KEY", Some("aws-inherited-secret")),
        ("LITELLM_API_KEY", None),
    ]);
    let directory = Directory::new();
    let home = directory.0.join("config");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("settings.json"),
        json!({"env":{"ANTHROPIC_BASE_URL":"https://gateway.example.test"}}).to_string(),
    )
    .unwrap();
    let script = directory.0.join("claude-fixture");
    std::fs::write(
        &script,
        r#"#!/usr/bin/python3
import json, os, sys
if sys.argv[1:] == ['agents', '--json']:
    print('[]')
    sys.exit(0)
home = os.environ['CLAUDE_CONFIG_DIR']
open(os.path.join(home, 'child-env.json'), 'w').write(json.dumps({
    key: os.environ.get(key)
    for key in ('ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'OPENAI_API_KEY', 'LITELLM_API_KEY', 'AWS_SECRET_ACCESS_KEY', 'PATH')
}))
for line in sys.stdin:
    value = json.loads(line)
    if value.get('type') == 'control_request' and value['request']['subtype'] == 'initialize':
        print(json.dumps({'type':'control_response','response':{'subtype':'success','request_id':value['request_id'],'response':{}}}), flush=True)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let claude = claude::Claude::new(
        script,
        home.clone(),
        directory.0.join("grants"),
        Url::parse("https://gateway.example.test").unwrap(),
    );
    let cwd = directory.0.to_str().unwrap();
    let absent = child_credentials(&claude, &home, cwd).await;
    assert_eq!(absent["ANTHROPIC_API_KEY"], Value::Null);
    assert_eq!(absent["ANTHROPIC_AUTH_TOKEN"], Value::Null);
    assert_eq!(absent["OPENAI_API_KEY"], Value::Null);
    assert_eq!(absent["LITELLM_API_KEY"], Value::Null);
    assert_eq!(absent["AWS_SECRET_ACCESS_KEY"], Value::Null);
    assert_eq!(absent["PATH"], std::env::var("PATH").unwrap());

    std::env::set_var("LITELLM_API_KEY", "sk-litellm-configured");
    let configured = child_credentials(&claude, &home, cwd).await;
    assert_eq!(configured["ANTHROPIC_API_KEY"], Value::Null);
    assert_eq!(configured["ANTHROPIC_AUTH_TOKEN"], "sk-litellm-configured");
    assert_eq!(configured["OPENAI_API_KEY"], Value::Null);
    assert_eq!(configured["LITELLM_API_KEY"], Value::Null);
    assert_eq!(configured["AWS_SECRET_ACCESS_KEY"], Value::Null);
}

async fn child_credentials(claude: &claude::Claude, home: &std::path::Path, cwd: &str) -> Value {
    let session = claude.create(cwd.into()).unwrap();
    let (_, id) = protocol::split_session(&session.id).unwrap();
    claude
        .send(id, "Check credentials".into(), None)
        .await
        .unwrap();
    let path = home.join("child-env.json");
    for _ in 0..50 {
        if path.exists() {
            return serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("Claude child did not record its environment");
}

struct CredentialEnv {
    saved: [(&'static str, Option<String>); 5],
}

impl CredentialEnv {
    fn set(values: &[(&str, Option<&str>)]) -> Self {
        let saved = [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "OPENAI_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "LITELLM_API_KEY",
        ]
        .map(|key| (key, std::env::var(key).ok()));
        for (key, value) in values {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        Self { saved }
    }
}

impl Drop for CredentialEnv {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[tokio::test]
#[ignore = "requires real Claude, an isolated LiteLLM on 127.0.0.1:4019 and LITELLM_API_KEY"]
async fn claude_approval_journal_live_probe() {
    let _env = CLAUDE_ENV.lock().await;
    use std::os::unix::fs::PermissionsExt;
    let root = PathBuf::from(
        std::env::var("CLAUDE_JOURNAL_LIVE_ROOT").expect("Set an isolated live probe directory"),
    );
    let directory = root.join(Uuid::new_v4().to_string());
    let home = directory.join("config");
    let grants = directory.join("grants");
    let project = directory.join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let gateway = Url::parse("http://127.0.0.1:4019").unwrap();
    std::fs::write(
        home.join("settings.json"),
        json!({"env":{"ANTHROPIC_BASE_URL":gateway}}).to_string(),
    )
    .unwrap();
    let executable = String::from_utf8(
        std::process::Command::new("which")
            .arg("claude")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    let wrapper = directory.join("claude-owner");
    std::fs::write(&wrapper, format!(r#"#!/usr/bin/python3
import os, pathlib, sys
session = next((arg.split('=', 1)[1] for arg in sys.argv if arg.startswith(('--session-id=', '--resume='))), None)
if session:
    (pathlib.Path(os.environ['CLAUDE_CONFIG_DIR']) / (session + '.pid')).write_text(str(os.getpid()))
os.execv({executable:?}, [{executable:?}] + sys.argv[1:])
"#)).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let make = || {
        claude::Claude::new(
            wrapper.clone(),
            home.clone(),
            grants.clone(),
            gateway.clone(),
        )
    };
    for allowed in [false, true] {
        let client = make();
        let session = client.create(project.to_str().unwrap().into()).unwrap();
        let id = Uuid::parse_str(session.id.trim_start_matches("claude:")).unwrap();
        let marker = project.join(format!("{id}.txt"));
        let command = format!(
            "printf 'approved\\n' >> {}{}",
            marker.display(),
            if allowed { "; sleep 8" } else { "" }
        );
        client.send(id, format!("Use only the Bash tool to execute exactly this command, then reply DONE. Do not use another tool or change the command. Command: {command}"), Some("claude-sonnet-5-5".into())).await.unwrap();
        let approval = tokio::time::timeout(Duration::from_secs(90), async {
            loop {
                let detail = client.read(id).await.unwrap();
                if let Some(approval) = detail.approvals.first() {
                    break approval.clone();
                }
                assert!(detail.error.is_none(), "{:?}", detail.error);
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        })
        .await
        .unwrap();
        let path = grants.join("approvals").join(format!("{id}.json"));
        let pending: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(pending[0]["state"], "pending");
        assert_eq!(pending[0]["summary"], approval.text);
        if allowed {
            client
                .approve(id, Uuid::parse_str(&approval.id).unwrap(), Decision::Accept)
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !marker.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(client.read(id).await.unwrap().session.can_stop);
            let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved[0]["state"], "allowed");
        }
        let pid: i32 = std::fs::read_to_string(home.join(format!("{id}.pid")))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        tokio::time::timeout(Duration::from_secs(10), async {
            while client.read(id).await.unwrap().session.can_stop {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        drop(client);
        let detail = make().read(id).await.unwrap();
        let serialized = serde_json::to_value(&detail).unwrap();
        assert_eq!(serialized.get("expiredApproval").is_some(), !allowed);
        assert_eq!(marker.exists(), allowed);
        if !allowed {
            assert_eq!(serialized["expiredApproval"]["id"], pending[0]["id"]);
            assert_eq!(serialized["expiredApproval"]["summary"], approval.text);
        }
        println!(
            "LIVE allowed={allowed} expiredApproval={} marker_exists={} session={id}",
            serialized.get("expiredApproval").is_some(),
            marker.exists()
        );
    }
    println!("LIVE artifacts={}", directory.display());
}

#[tokio::test]
async fn claude_approval_journal_survives_restart_without_misreporting_executed_tools() {
    let _env = CLAUDE_ENV.lock().await;
    use std::os::unix::fs::PermissionsExt;
    let directory = Directory::new();
    let home = directory.0.join("config");
    let grants = directory.0.join("grants");
    let project = home.join("projects/fixture");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        home.join("settings.json"),
        json!({"env":{"ANTHROPIC_BASE_URL":"https://gateway.example.test"}}).to_string(),
    )
    .unwrap();
    let script = directory.0.join("claude-fixture");
    std::fs::write(&script, r#"#!/usr/bin/python3
import json, os, pathlib, sys
if sys.argv[1:] == ['agents', '--json']:
    print('[]')
    sys.exit(0)
session = next(arg.split('=', 1)[1] for arg in sys.argv if arg.startswith(('--session-id=', '--resume=')))
home = pathlib.Path(os.environ['CLAUDE_CONFIG_DIR'])
(home / (session + '.pid')).write_text(str(os.getpid()))
path = home / 'projects' / 'fixture' / (session + '.jsonl')
def emit(value):
    print(json.dumps(value), flush=True)
def append(value):
    with path.open('a') as output:
        output.write(json.dumps(value) + '\n')
for line in sys.stdin:
    value = json.loads(line)
    if value['type'] == 'control_request':
        emit({'type':'control_response','response':{'subtype':'success','request_id':value['request_id'],'response':{}}})
        if value['request']['subtype'] == 'interrupt':
            emit({'type':'result','subtype':'success'})
    elif value['type'] == 'user':
        mode = value['message']['content']
        append(value)
        if mode == 'cannot-persist':
            journal = home.parent / 'grants' / 'approvals'
            journal.rename(home.parent / 'grants' / 'saved-approvals')
            journal.write_text('not a directory')
        if mode != 'unflushed':
            append({'type':'assistant','message':{'content':[{'type':'tool_use','id':'tool-' + session,'name':'Bash','input':{'command':'echo ok'}}]}})
        emit({'type':'control_request','request_id':'permission','request':{'subtype':'can_use_tool','tool_use_id':'tool-' + session,'tool_name':'Bash','input':{'command':'echo ok'}}})
    elif value['type'] == 'control_response':
        records = json.loads((home.parent / 'grants' / 'approvals' / (session + '.json')).read_text())
        (home / (session + '.forwarded')).write_text(records[0]['state'])
        if mode == 'completed' or value['response']['response']['behavior'] == 'deny':
            append({'type':'user','message':{'content':[{'type':'tool_result','tool_use_id':'tool-' + session,'content':'done'}]}})
            emit({'type':'result','subtype':'success'})
"#).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let make = || {
        claude::Claude::new(
            script.clone(),
            home.clone(),
            grants.clone(),
            Url::parse("https://gateway.example.test").unwrap(),
        )
    };
    for mode in [
        "pending",
        "unflushed",
        "allowed",
        "declined",
        "stop",
        "completed",
        "later",
        "corrupt",
        "expired",
        "cannot-decide",
        "new-turn",
    ] {
        let client = make();
        let session = client.create(directory.0.to_str().unwrap().into()).unwrap();
        let id = Uuid::parse_str(session.id.trim_start_matches("claude:")).unwrap();
        client.send(id, mode.into(), None).await.unwrap();
        let detail = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let detail = client.read(id).await.unwrap();
                if !detail.approvals.is_empty() {
                    break detail;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(serde_json::to_value(&detail).unwrap()["expiredApproval"].is_null());
        let journal = grants.join("approvals").join(format!("{id}.json"));
        let summary = detail.approvals[0].text.clone();
        let approval = Uuid::parse_str(&detail.approvals[0].id).unwrap();
        if ["allowed", "declined", "completed"].contains(&mode) {
            client
                .approve(
                    id,
                    approval,
                    if mode == "declined" {
                        Decision::Decline
                    } else {
                        Decision::Accept
                    },
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), async {
                while !home.join(format!("{id}.forwarded")).exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                std::fs::read_to_string(home.join(format!("{id}.forwarded"))).unwrap(),
                if mode == "declined" {
                    "declined"
                } else {
                    "allowed"
                }
            );
        }
        if mode == "stop" {
            client.interrupt(id).await.unwrap();
        }
        if ["stop", "declined", "completed"].contains(&mode) {
            tokio::time::timeout(Duration::from_secs(5), async {
                while client.read(id).await.unwrap().session.can_stop {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert!(!journal.exists());
        }
        if mode == "cannot-decide" {
            std::fs::write(&journal, "not json").unwrap();
            assert!(client
                .approve(id, approval, Decision::Accept)
                .await
                .is_err());
            assert!(!home.join(format!("{id}.forwarded")).exists());
        }
        let pid: i32 = std::fs::read_to_string(home.join(format!("{id}.pid")))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        tokio::time::timeout(Duration::from_secs(5), async {
            while client.read(id).await.unwrap().session.can_stop {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        drop(client);
        if mode == "later" {
            use std::io::Write;
            writeln!(std::fs::OpenOptions::new().append(true).open(project.join(format!("{id}.jsonl"))).unwrap(), "{}", json!({"type":"user","uuid":Uuid::new_v4(),"message":{"content":[{"type":"text","text":"A later turn"}]}})).unwrap();
        }
        if mode == "corrupt" {
            std::fs::write(&journal, "{broken").unwrap();
        }
        if mode == "expired" {
            let mut old: Value = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
            old[0]["createdAt"] = json!(protocol::now_ms() - 8 * 24 * 60 * 60 * 1000);
            std::fs::write(&journal, old.to_string()).unwrap();
        }
        let restored = make();
        let detail = restored.read(id).await.unwrap();
        let value = serde_json::to_value(&detail).unwrap();
        if ["pending", "unflushed", "new-turn"].contains(&mode) {
            assert_eq!(
                value["expiredApproval"],
                json!({"id":format!("tool-{id}"),"tool":"Bash","summary":"Bash\n{\n  \"command\": \"echo ok\"\n}"}),
                "Pending approval was silently lost after restart ({mode})"
            );
        } else {
            assert!(
                value.get("expiredApproval").is_none(),
                "Unexpected expired approval for {mode}"
            );
        }
        if mode == "pending" {
            assert_eq!(
                std::fs::metadata(&journal).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let record: Value = serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
            assert_eq!(record[0]["state"], "pending");
            assert_eq!(record[0]["summary"], summary);
        }
        if mode == "new-turn" {
            restored.send(id, "unflushed".into(), None).await.unwrap();
            assert!(serde_json::to_value(restored.read(id).await.unwrap())
                .unwrap()
                .get("expiredApproval")
                .is_none());
        }
    }
    let client = make();
    let session = client.create(directory.0.to_str().unwrap().into()).unwrap();
    let id = Uuid::parse_str(session.id.trim_start_matches("claude:")).unwrap();
    assert!(serde_json::to_value(client.read(id).await.unwrap())
        .unwrap()
        .get("expiredApproval")
        .is_none());
    client
        .send(id, "cannot-persist".into(), None)
        .await
        .unwrap();
    let failed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let detail = client.read(id).await.unwrap();
            if !detail.session.can_stop {
                break detail;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        failed.approvals.is_empty(),
        "Never expose an approval whose pending record could not be saved"
    );
    assert!(failed
        .error
        .unwrap()
        .contains("Cannot safely save Claude approval state"));
}

#[tokio::test]
async fn claude_native_protocol_handoff_approvals_stop_and_route_changes() {
    let _env = CLAUDE_ENV.lock().await;
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
        if value['message']['content'] == 'Stream blocks':
            emit({'type':'system','subtype':'init','session_id':session,'model':'fixture-model'})
            emit({'type':'stream_event','event':{'type':'message_start','message':{'id':'blocks'}}})
            for piece in ['First ', 'part']:
                emit({'type':'stream_event','event':{'type':'content_block_delta','index':1,'delta':{'type':'text_delta','text':piece}}})
            second = 'Second block ' + 'x' * 40000 + ' sk-aaaaaaaaaa'
            for start in range(0, len(second), 64):
                emit({'type':'stream_event','event':{'type':'content_block_delta','index':3,'delta':{'type':'text_delta','text':second[start:start + 64]}}})
            emit({'type':'stream_event','event':{'type':'content_block_delta','index':3,'delta':{'type':'text_delta','text':'aaaaaaaaaa done'}}})
            second += 'aaaaaaaaaa done'
            (home / 'streamed').touch()
            while not (home / 'finish-block').exists():
                time.sleep(0.01)
            emit({'type':'stream_event','event':{'type':'content_block_stop','index':3}})
            while not (home / 'complete-stream').exists():
                time.sleep(0.01)
            reply = {'type':'assistant','uuid':'blocks-complete','message':{'id':'blocks','content':[{'type':'text','text':'First part'},{'type':'tool_use','id':'blocks-tool','name':'Edit','input':{'file_path':'fixture.txt'}},{'type':'text','text':second}]}}
            project = home / 'projects' / 'fixture'
            project.mkdir(parents=True, exist_ok=True)
            with (project / (session + '.jsonl')).open('a') as transcript:
                transcript.write(json.dumps(reply) + '\n')
            emit(reply)
            emit({'type':'result','subtype':'success','session_id':session})
            continue
        reported = '00000000-0000-4000-8000-000000000001' if value['message']['content'] == 'Mismatch' else session
        emit({'type':'system','subtype':'init','session_id':reported,'model':'fixture-model'})
        emit({'type':'stream_event','event':{'type':'message_start','message':{'id':'reply'}}})
        emit({'type':'assistant','uuid':'thinking','message':{'id':'reply','content':[{'type':'thinking','thinking':'Hidden'}]}})
        emit({'type':'stream_event','event':{'type':'content_block_delta','index':1,'delta':{'type':'text_delta','text':'Live Claude reply'}}})
        emit({'type':'control_request','request_id':'permission','request':{'subtype':'can_use_tool','tool_use_id':'fixture-tool','tool_name':'Edit','input':{'file_path':'fixture.txt','old_string':'before','new_string':'after'}}})
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
    std::fs::write(project.join(format!("{existing}.jsonl")), format!("not json\n{{\"type\":\"file-history-snapshot\",\"snapshot\":\"truncated\n{}\n{}\n{{\"type\":\"user\",\"message\":\n{}\n{}\n{}\n{}\n",
        json!({"type":"file-history-snapshot","snapshot":"x".repeat(70_000)}),
        json!({"type":"user","cwd":cwd,"isMeta":true,"message":{"content":"Hidden setup"}}),
        json!({"type":"user","cwd":cwd,"uuid":"original","message":{"content":"Original context"}}),
        json!({"type":"file-history-snapshot","snapshot":"x".repeat(4 * 1024 * 1024)}),
        json!({"type":"assistant","uuid":"chunk-a","message":{"id":"multi-block","content":[{"type":"text","text":"Repeated on purpose"}]}}),
        json!({"type":"assistant","uuid":"chunk-b","message":{"id":"multi-block","content":[{"type":"text","text":"Repeated on purpose"}]}}),
    )).unwrap();
    assert!(reopened
        .handoff_choices()
        .await
        .unwrap()
        .iter()
        .any(|choice| choice.id == format!("claude:{existing}")
            && choice.title == "Original context"));
    std::fs::write(
        home.join("active.json"),
        json!([{"sessionId":existing,"pid":1,"cwd":cwd,"name":"Different CLI label"}]).to_string(),
    )
    .unwrap();
    assert_eq!(claude.handoff(existing).await.unwrap_err().status, 409);
    assert!(!claude
        .handoff_choices()
        .await
        .unwrap()
        .iter()
        .any(|choice| choice.id == format!("claude:{existing}")));
    let phone_title = claude
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|session| session.id == format!("claude:{existing}"))
        .unwrap()
        .title;
    assert_eq!(phone_title, "Original context");
    let transcript = project.join(format!("{existing}.jsonl"));
    let original = std::fs::read_to_string(&transcript).unwrap();
    let compacted = original
        .lines()
        .filter(|line| {
            !serde_json::from_str::<Value>(line).is_ok_and(|row| row["uuid"] == "original")
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&transcript, format!("{compacted}\n")).unwrap();
    assert_eq!(
        claude
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|session| session.id == format!("claude:{existing}"))
            .unwrap()
            .title,
        phone_title
    );
    std::fs::write(home.join("active.json"), "[]").unwrap();
    for _ in 0..20 {
        let file =
            std::fs::File::create(project.join(format!("{}.jsonl", Uuid::new_v4()))).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::SystemTime::now() + Duration::from_secs(10)),
        )
        .unwrap();
    }
    let choices = claude.handoff_choices().await.unwrap();
    let choice = choices
        .iter()
        .position(|choice| choice.id == format!("claude:{existing}"))
        .unwrap();
    assert_eq!(choices[choice].title, phone_title);
    let mut output = Vec::new();
    let selected = super::choose_handoff(
        &choices,
        &mut std::io::Cursor::new(format!("invalid\n{}\n", choice + 1)),
        &mut output,
    )
    .unwrap();
    assert_eq!(selected, existing);
    let display = String::from_utf8(output).unwrap();
    assert!(display.contains("Original context"));
    assert!(!display.contains(&existing.to_string()));
    assert!(
        super::choose_handoff(&choices, &mut std::io::Cursor::new("0\n"), &mut Vec::new()).is_err()
    );
    claude.handoff(selected).await.unwrap();
    assert_eq!(
        claude.read(existing).await.unwrap().session.title,
        phone_title
    );
    let repeated = claude.read(existing).await.unwrap();
    assert!(repeated.older_messages);
    assert_eq!(repeated.messages.len(), 2);
    assert_eq!(repeated.messages[0].text, repeated.messages[1].text);
    assert_ne!(repeated.messages[0].id, repeated.messages[1].id);
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
    let fresh = claude::Claude::new(
        script.clone(),
        home.clone(),
        directory.0.join("grants"),
        Url::parse("https://gateway.example.test").unwrap(),
    );
    let streaming = fresh.create(cwd.into()).unwrap();
    let (_, streaming_id) = protocol::split_session(&streaming.id).unwrap();
    fresh
        .send(streaming_id, "Stream blocks".into(), None)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !home.join("streamed").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut streamed = fresh.read(streaming_id).await.unwrap();
    for _ in 0..200 {
        if streamed
            .messages
            .iter()
            .any(|message| message.text.ends_with("] done"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        streamed = fresh.read(streaming_id).await.unwrap();
    }
    assert_eq!(streamed.session.status, "working");
    assert_eq!(streamed.streaming_reply_id.as_deref(), Some("blocks/1"));
    let live = streamed
        .messages
        .iter()
        .filter(|message| message.role == "assistant")
        .collect::<Vec<_>>();
    assert_eq!(live.len(), 2, "each streamed text block is one reply row");
    assert_eq!(live[0].text, "First part");
    assert!(live[1].text.starts_with("Second block"));
    assert!(live[1].text.ends_with("[credential redacted] done"));
    assert!(!live[1].text.contains("sk-a"));
    std::fs::write(home.join("finish-block"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let detail = fresh.read(streaming_id).await.unwrap();
            assert!(detail.session.can_stop);
            if detail.streaming_reply_id.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(home.join("complete-stream"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while fresh.read(streaming_id).await.unwrap().session.status == "working" {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let saved = fresh.read(streaming_id).await.unwrap();
    let replies = saved
        .messages
        .iter()
        .filter(|message| message.role == "assistant")
        .collect::<Vec<_>>();
    assert_eq!(replies.len(), 2, "completion replaces streamed rows");
    assert_eq!(replies[0].text, "First part");
    assert!(replies[1].text.starts_with("Second block"));
    assert!(replies[1].text.ends_with("[credential redacted] done"));
    assert!(!replies[1].text.contains("sk-a"));
    assert!(saved
        .messages
        .iter()
        .any(|message| message.role == "activity"));
}
