use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, protocol::WebSocketConfig, Message,
};
use url::Url;
use uuid::Uuid;

use super::{
    auth::{http_client, response_json},
    claude::Claude,
    codex::Codex,
    protocol::{self, now_ms, Outcome, Reply},
    sessions::Sessions,
};

pub struct DeviceAccess {
    credential: String,
    cached: Option<(String, i64, i64)>,
}

impl DeviceAccess {
    pub fn new(credential: impl Into<String>) -> Self {
        Self {
            credential: credential.into(),
            cached: None,
        }
    }

    pub fn needs_refresh(&self, now: i64) -> bool {
        let Some((_, issued, expires)) = self.cached else {
            return true;
        };
        let ttl = expires.saturating_sub(issued);
        now >= issued.saturating_add(ttl.saturating_mul(4) / 5)
    }

    pub fn reject(&mut self) {
        self.cached = None;
    }

    pub fn remember(&mut self, token: impl Into<String>, now: i64, expires_at: i64) {
        self.cached = Some((token.into(), now, expires_at));
    }

    fn cached_token(&self, now: i64) -> Option<String> {
        (!self.needs_refresh(now)).then(|| self.cached.as_ref().unwrap().0.clone())
    }
}

pub async fn ensure_device_access(
    endpoint: &Url,
    access: &mut DeviceAccess,
    now: i64,
) -> Result<(String, i64)> {
    if let Some(token) = access.cached_token(now) {
        let expires = access.cached.as_ref().unwrap().2;
        return Ok((token, expires));
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Exchanged {
        token: String,
        expires_at: i64,
    }
    let exchanged: Exchanged = post(
        endpoint,
        "/api/auth/device",
        json!({}),
        Some(&access.credential),
    )
    .await?;
    access.remember(exchanged.token.clone(), now, exchanged.expires_at);
    Ok((exchanged.token, exchanged.expires_at))
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Credentials {
    endpoint: Url,
    device_id: Uuid,
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
    let request = http_client()
        .post(endpoint.join(path)?)
        .header("X-LiteLLM-Client", "relay")
        .json(&body);
    let request = match token {
        Some(token) => request.bearer_auth(token),
        None => request,
    };
    response_json(
        request
            .send()
            .await
            .context("Cannot reach the remote service.")?,
    )
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
    println!("Sign in to pair this Mac. Verification code {}", start.code);
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
                let device: Device = post(
                    &endpoint,
                    "/api/devices",
                    json!({"name":name.unwrap_or_else(crate::system::hostname)}),
                    Some(&token),
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
                println!("This Mac is paired. Run relay remote connect and sign in to the same remote service in the mobile app.");
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
            .context("Invalid pairing record. Pair this Mac again.")?;
    super::address(Some(credentials.endpoint.clone()), "LITELLM_REMOTE_URL")?;
    super::address(Some(credentials.gateway.clone()), "LITELLM_GATEWAY_URL")?;
    anyhow::ensure!(
        credentials.endpoint.path() == "/" && credentials.device_token.len() == 43,
        "Invalid pairing record. Pair this Mac again."
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
    Sessions {
        codex: Codex::new(
            codex_socket
                .unwrap_or_else(|| codex_home.join("app-server-control/app-server-control.sock")),
            provider,
            credentials.gateway.clone(),
        ),
        claude: Claude::new(
            executable,
            claude_home,
            credentials_file()
                .parent()
                .unwrap_or(Path::new("."))
                .join(format!("claude-{}", credentials.device_id)),
            credentials.gateway.clone(),
        ),
    }
}

pub async fn connect(credentials: Credentials, sessions: Sessions) -> Result<()> {
    let sessions = Arc::new(sessions);
    let mut access = DeviceAccess::new(credentials.device_token.clone());
    let mut delay = 1;
    let mut rejected = false;
    loop {
        if rejected {
            access.reject();
        }
        let now = now_ms();
        let opened = ensure_device_access(&credentials.endpoint, &mut access, now).await;
        let (token, _) = match opened {
            Ok(value) => value,
            Err(error) => {
                eprintln!("{error}");
                println!("Remote service disconnected. Reconnecting.");
                tokio::select! { _ = tokio::time::sleep(Duration::from_secs(delay)) => {}, _ = tokio::signal::ctrl_c() => return Ok(()) }
                delay = (delay * 2).min(30);
                rejected = false;
                continue;
            }
        };
        let refresh_at = access
            .cached
            .as_ref()
            .map(|(_, issued, expires)| issued + (expires - issued) * 4 / 5)
            .unwrap_or(now);
        let connected = connection(&credentials, &token, sessions.clone());
        let wait = Duration::from_millis(refresh_at.saturating_sub(now).max(1) as u64);
        tokio::select! {
            result = connected => {
                match result {
                    Ok(()) => { delay = 1; rejected = false; }
                    Err(error) if error.downcast_ref::<RejectedToken>().is_some() && !rejected => { rejected = true; continue; }
                    Err(error) => { eprintln!("{error}"); rejected = false; }
                }
            }
            _ = tokio::time::sleep(wait) => { rejected = false; continue; }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
        println!("Remote service disconnected. Reconnecting.");
        tokio::select! { _ = tokio::time::sleep(Duration::from_secs(delay)) => {}, _ = tokio::signal::ctrl_c() => return Ok(()) }
        delay = (delay * 2).min(30);
    }
}

#[derive(Debug)]
struct RejectedToken;

impl std::fmt::Display for RejectedToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("The remote service rejected the access token.")
    }
}

impl std::error::Error for RejectedToken {}

async fn connection(credentials: &Credentials, token: &str, sessions: Arc<Sessions>) -> Result<()> {
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
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse()?);
    let config = WebSocketConfig::default().max_message_size(Some(131_072));
    let opened = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(request, Some(config), false),
    )
    .await;
    let (socket, _) = opened
        .map_err(|_| anyhow::anyhow!("The remote service timed out."))?
        .map_err(|error| {
            if let tokio_tungstenite::tungstenite::Error::Http(response) = &error {
                if response.status().as_u16() == 401 {
                    return anyhow::Error::from(RejectedToken);
                }
            }
            anyhow::anyhow!("Cannot connect to the remote service. Check the address and pairing.")
        })?;
    println!("Connected to LiteLLM Remote. Keep this Mac awake.");
    let (mut sink, mut source) = socket.split();
    let mut work = tokio::task::JoinSet::new();
    let mut seen = HashMap::<Uuid, ()>::new();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
    let mut last = tokio::time::Instant::now();
    loop {
        tokio::select! {
            frame = source.next() => {
                last = tokio::time::Instant::now();
                match frame {
                    Some(Ok(Message::Text(text))) => {
                        let command: protocol::Command = serde_json::from_str(&text).context("The service sent an unsupported command.")?;
                        if seen.contains_key(&command.id) { continue; }
                        if seen.len() >= 4096 { return Ok(()); }
                        seen.insert(command.id, ());
                        if work.len() >= 16 {
                            let reply = Reply { id: command.id, result: Outcome::from_result(Err(protocol::Failure::new(429, "The Mac is busy. Try again shortly."))) };
                            sink.send(Message::Text(serde_json::to_string(&reply)?.into())).await?;
                            continue;
                        }
                        let sessions = sessions.clone();
                        work.spawn(async move { Reply { id: command.id, result: Outcome::from_result(sessions.execute(command.action).await) } });
                    }
                    Some(Ok(Message::Ping(bytes))) => sink.send(Message::Pong(bytes)).await?,
                    Some(Ok(Message::Pong(_))) => {}
                    _ => return Ok(()),
                }
            }
            reply = work.join_next(), if !work.is_empty() => {
                let reply = reply.context("The Mac request ended unexpectedly.")??;
                let bytes = serde_json::to_string(&reply)?;
                if bytes.len() > 4 * 1024 * 1024 { anyhow::bail!("The session response exceeded the transport limit."); }
                sink.send(Message::Text(bytes.into())).await?;
            }
            _ = heartbeat.tick() => {
                if last.elapsed() > Duration::from_secs(90) { return Ok(()); }
                sink.send(Message::Ping(Vec::new().into())).await?;
            }
        }
    }
}
