use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use futures_util::{Sink, SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, Error as WebSocketError, Message,
};
use tokio_tungstenite::WebSocketStream;
use url::Url;
use uuid::Uuid;

use super::{
    auth::{http_client, response_json},
    claude::Claude,
    codex::Codex,
    protocol::{self, Outcome, Reply},
    sessions::Sessions,
};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Credentials {
    endpoint: Url,
    pub(super) device_id: Uuid,
    device_token: String,
    gateway: Url,
}

fn credentials_file() -> PathBuf {
    crate::config::relay_home().join("remote/device.json")
}

async fn post<T: DeserializeOwned>(
    endpoint: &Url,
    path: &str,
    body: Value,
    token: Option<&str>,
) -> Result<T> {
    response_json(post_request(endpoint, path, body, token).await?)
        .await
        .map_err(|error| anyhow::anyhow!(error.error))
}

async fn post_request(
    endpoint: &Url,
    path: &str,
    body: Value,
    token: Option<&str>,
) -> Result<reqwest::Response> {
    let request = http_client()
        .post(endpoint.join(path)?)
        .header("X-LiteLLM-Client", "relay")
        .json(&body);
    let request = match token {
        Some(token) => request.bearer_auth(token),
        None => request,
    };
    request
        .send()
        .await
        .context("Cannot reach the remote service.")
}

async fn enroll<T: DeserializeOwned>(endpoint: &Url, name: String, token: &str) -> Result<T> {
    let path = "/api/devices";
    let mut response = post_request(
        endpoint,
        path,
        json!({"name":name,"platform":protocol::Platform::current()}),
        Some(token),
    )
    .await?;
    if response.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
        let mut error = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .context("Cannot read the pairing response.")?
        {
            if chunk.len() > 4096 - error.len() {
                break;
            }
            error.extend_from_slice(&chunk);
        }
        if String::from_utf8_lossy(&error).contains("unknown field `platform`") {
            eprintln!("The remote service does not support device platforms. Upgrade it to identify this device correctly.");
            return post(endpoint, path, json!({"name":name}), Some(token)).await;
        }
    }
    response_json(response)
        .await
        .map_err(|error| anyhow::anyhow!(error.error))
}

pub async fn pair(endpoint: Url, name: Option<String>) -> Result<()> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Start {
        flow_token: String,
        code: String,
        verification_url: Url,
    }
    #[derive(Deserialize)]
    struct Team {
        id: String,
        name: String,
    }
    #[derive(Deserialize)]
    #[serde(tag = "status", rename_all = "lowercase")]
    enum Poll {
        Pending,
        Teams { teams: Vec<Team> },
        Ready { token: String },
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Device {
        device_id: Uuid,
        device_token: String,
        gateway: Url,
    }
    let start: Start = post(&endpoint, "/api/auth/start", json!({}), None).await?;
    anyhow::ensure!(
        start.verification_url.scheme() == "https"
            || matches!(
                start.verification_url.host_str(),
                Some("localhost" | "127.0.0.1" | "[::1]")
            ),
        "Invalid sign-in address."
    );
    println!(
        "Sign in to pair this device. Verification code {}",
        start.code
    );
    println!("{}", start.verification_url);
    crate::auth::open_browser(start.verification_url.as_str());
    let mut team: Option<String> = None;
    for _ in 0..200 {
        let result: Poll = post(
            &endpoint,
            "/api/auth/poll",
            json!({"flowToken":&start.flow_token,"team":&team}),
            None,
        )
        .await?;
        match result {
            Poll::Pending => tokio::time::sleep(Duration::from_secs(3)).await,
            Poll::Teams { teams } => {
                for (index, team) in teams.iter().enumerate() {
                    println!("{}. {}", index + 1, team.name);
                }
                let selected = tokio::task::spawn_blocking(|| -> Result<usize> {
                    print!("Choose your team number ");
                    std::io::stdout().flush()?;
                    let mut input = String::new();
                    std::io::stdin().read_line(&mut input)?;
                    input
                        .trim()
                        .parse::<usize>()
                        .context("Enter a team number.")
                })
                .await??;
                team = Some(
                    teams
                        .get(selected.checked_sub(1).context("Enter a team number.")?)
                        .context("Unknown team.")?
                        .id
                        .clone(),
                );
            }
            Poll::Ready { token } => {
                let device: Device = enroll(
                    &endpoint,
                    name.unwrap_or_else(crate::system::hostname),
                    &token,
                )
                .await?;
                let _: Value = post(&endpoint, "/api/auth/logout", json!({}), Some(&token)).await?;
                let gateway = super::address(Some(device.gateway), "LITELLM_GATEWAY_URL")?;
                let credentials = Credentials {
                    endpoint,
                    device_id: device.device_id,
                    device_token: device.device_token,
                    gateway,
                };
                crate::system::write_private(
                    &credentials_file(),
                    &serde_json::to_string(&credentials)?,
                )?;
                println!("This device is paired. Run relay remote connect and sign in to the same remote service in the mobile app.");
                return Ok(());
            }
        }
    }
    anyhow::bail!("Sign-in expired. Run relay remote pair again.")
}

pub fn load() -> Result<Credentials> {
    let file = credentials_file();
    let credentials: Credentials =
        serde_json::from_slice(&std::fs::read(file).context("Run relay remote pair first.")?)
            .context("Invalid pairing record. Pair this device again.")?;
    super::address(Some(credentials.endpoint.clone()), "LITELLM_REMOTE_URL")?;
    super::address(Some(credentials.gateway.clone()), "LITELLM_GATEWAY_URL")?;
    anyhow::ensure!(
        credentials.endpoint.path() == "/" && credentials.device_token.len() == 43,
        "Invalid pairing record. Pair this device again."
    );
    Ok(credentials)
}

pub fn sessions(
    credentials: &Credentials,
    codex_socket: Option<PathBuf>,
    claude_executable: Option<PathBuf>,
    provider: String,
) -> Sessions {
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::system::home_dir().join(".codex"));
    let claude_home = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::system::home_dir().join(".claude"));
    let executable = claude_executable
        .or_else(|| {
            crate::ai_tools::claude_cli::claude_binary(
                std::env::var_os("PATH").as_deref(),
                &crate::system::home_dir(),
            )
        })
        .unwrap_or_else(|| PathBuf::from("claude"));
    let auto_start_codex = cfg!(target_os = "linux") && codex_socket.is_none();
    Sessions {
        codex: Codex::new(
            codex_socket
                .unwrap_or_else(|| codex_home.join("app-server-control/app-server-control.sock")),
            provider,
            credentials.gateway.clone(),
        )
        .auto_start(auto_start_codex),
        claude: Claude::new(
            executable,
            claude_home,
            credentials_file()
                .parent()
                .unwrap_or(Path::new("."))
                .join(format!("claude-{}", credentials.device_id)),
            credentials.gateway.clone(),
        )
        .into(),
    }
}

pub async fn connect(credentials: Credentials, sessions: Sessions) -> Result<()> {
    let sessions = Arc::new(sessions);
    let mut delay = 1;
    loop {
        let connected = connection(&credentials, sessions.clone());
        tokio::select! {
            result = connected => {
                match result {
                    Ok(()) => { delay = 1; }
                    Err(error) => { eprintln!("{error}"); }
                }
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
        println!("Remote service disconnected. Reconnecting.");
        tokio::select! { _ = tokio::time::sleep(Duration::from_secs(delay)) => {}, _ = tokio::signal::ctrl_c() => return Ok(()) }
        delay = (delay * 2).min(30);
    }
}

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(60);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

async fn send_frame(
    sink: &mut (impl Sink<Message, Error = WebSocketError> + Unpin),
    message: Message,
    deadline: tokio::time::Instant,
) -> Result<()> {
    let deadline = deadline.min(tokio::time::Instant::now() + WRITE_TIMEOUT);
    tokio::time::timeout_at(deadline, sink.send(message))
        .await
        .context("The remote service link stopped accepting writes.")??;
    Ok(())
}

async fn connection(credentials: &Credentials, sessions: Arc<Sessions>) -> Result<()> {
    let mut endpoint = credentials.endpoint.join("/agent")?;
    let scheme = if endpoint.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    endpoint
        .set_scheme(scheme)
        .map_err(|_| anyhow::anyhow!("Invalid service address."))?;
    let mut request = endpoint.as_str().into_client_request()?;
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", credentials.device_token).parse()?,
    );
    let config = WebSocketConfig::default().max_message_size(Some(131_072));
    let opened = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(request, Some(config), false),
    )
    .await;
    let (socket, _) = opened
        .map_err(|_| anyhow::anyhow!("The remote service timed out."))?
        .map_err(|_| {
            anyhow::anyhow!("Cannot connect to the remote service. Check the address and pairing.")
        })?;
    println!("Connected to LiteLLM Remote. Keep this device online.");
    serve_link(socket, sessions).await
}

async fn serve_link(
    socket: WebSocketStream<impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>,
    sessions: Arc<Sessions>,
) -> Result<()> {
    let (mut sink, mut source) = socket.split();
    let mut work = tokio::task::JoinSet::new();
    let mut seen = HashMap::<Uuid, ()>::new();
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut deadline = tokio::time::Instant::now() + HEARTBEAT_TIMEOUT;
    let mut pending_ping = None;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                anyhow::bail!("The remote service link stopped responding.");
            }
            frame = source.next() => {
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        let command: protocol::Command = serde_json::from_str(&text).context("The service sent an unsupported command.")?;
                        if seen.contains_key(&command.id) { continue; }
                        if seen.len() >= 4096 { return Ok(()); }
                        seen.insert(command.id, ());
                        if work.len() >= 16 {
                            let reply = Reply { id: command.id, result: Outcome::from_result(Err(protocol::Failure::new(429, "The device is busy. Try again shortly."))) };
                            send_frame(&mut sink, Message::Text(serde_json::to_string(&reply)?.into()), deadline).await?;
                            continue;
                        }
                        let sessions = sessions.clone();
                        work.spawn(async move { Reply { id: command.id, result: Outcome::from_result(sessions.execute(command.action).await) } });
                    }
                    Some(Ok(Message::Ping(bytes))) => {
                        send_frame(&mut sink, Message::Pong(bytes), deadline).await?;
                    }
                    Some(Ok(Message::Pong(bytes))) => {
                        if pending_ping.as_ref().is_some_and(|ping: &Uuid| bytes.as_ref() == ping.as_bytes()) {
                            pending_ping = None;
                            deadline = tokio::time::Instant::now() + HEARTBEAT_TIMEOUT;
                        }
                    }
                    _ => return Ok(()),
                }
            }
            reply = work.join_next(), if !work.is_empty() => {
                let reply = reply.context("The device request ended unexpectedly.")??;
                let bytes = serde_json::to_string(&reply)?;
                if bytes.len() > 4 * 1024 * 1024 { anyhow::bail!("The session response exceeded the transport limit."); }
                send_frame(&mut sink, Message::Text(bytes.into()), deadline).await?;
            }
            _ = heartbeat.tick(), if pending_ping.is_none() => {
                let ping = Uuid::new_v4();
                send_frame(&mut sink, Message::Ping(ping.as_bytes().to_vec().into()), deadline).await?;
                pending_ping = Some(ping);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
    use tokio::time::Instant;
    use tokio_tungstenite::tungstenite::protocol::Role;

    #[tokio::test]
    async fn pairing_only_omits_platform_when_the_service_rejects_that_field() {
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let app = Router::new().route(
            "/api/devices",
            post(move |Json(body): Json<Value>| {
                let recorded = recorded.clone();
                async move {
                    recorded.lock().unwrap().push(body.clone());
                    match body["name"].as_str().unwrap() {
                        "old" if body.get("platform").is_some() => (
                            StatusCode::UNPROCESSABLE_ENTITY,
                            "unknown field `platform`, expected `name`",
                        )
                            .into_response(),
                        "invalid" => {
                            (StatusCode::UNPROCESSABLE_ENTITY, "invalid name").into_response()
                        }
                        _ => Json(json!({"paired":true})).into_response(),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for name in ["old", "new"] {
            assert_eq!(
                enroll::<Value>(&endpoint, name.into(), "token")
                    .await
                    .unwrap(),
                json!({"paired":true})
            );
        }
        assert!(enroll::<Value>(&endpoint, "invalid".into(), "token")
            .await
            .is_err());
        let platform = protocol::Platform::current();
        assert_eq!(
            *requests.lock().unwrap(),
            [
                json!({"name":"old","platform":platform}),
                json!({"name":"old"}),
                json!({"name":"new","platform":platform}),
                json!({"name":"invalid","platform":platform}),
            ]
        );
        server.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn reconnects_when_heartbeat_is_not_acknowledged() {
        let gateway = Url::parse("https://example.test").unwrap();
        let sessions = Arc::new(Sessions {
            claude: Claude::new(
                "unused".into(),
                "unused".into(),
                "unused".into(),
                gateway.clone(),
            )
            .into(),
            codex: Codex::new("unused".into(), "litellm".into(), gateway),
        });
        let (connector, peer) = tokio::io::duplex(1024);
        let connector = WebSocketStream::from_raw_socket(connector, Role::Client, None).await;
        let mut peer = WebSocketStream::from_raw_socket(peer, Role::Server, None).await;
        let link = tokio::spawn(serve_link(connector, sessions));
        let Some(Ok(Message::Ping(ping))) = peer.next().await else {
            panic!("expected connector ping");
        };
        peer.send(Message::Pong(ping)).await.unwrap();
        let Some(Ok(Message::Ping(_))) = peer.next().await else {
            panic!("expected another ping after acknowledgement");
        };
        peer.send(Message::Pong(Vec::new().into())).await.unwrap();
        let sent_at = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(35), link)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            result.unwrap_err().to_string(),
            "The remote service link stopped responding."
        );
        assert_eq!(sent_at.elapsed(), Duration::from_secs(30));
    }
}
