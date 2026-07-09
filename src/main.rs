use std::{
    collections::{HashMap, VecDeque},
    env,
    fs::{self, OpenOptions},
    io::{Cursor, Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use rustls::{pki_types::ServerName, ClientConfig, RootCertStore, ServerConfig};
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use uuid::Uuid;

const DEFAULT_NOTION_DOMAINS: &[&str] = &[
    "notion.so",
    "notion.com",
    "api.notion.com",
    "www.notion.so",
    "app.notion.com",
];

const DEFAULT_AI_DOMAINS: &[&str] = &[
    "notion.so",
    "notion.com",
    "api.notion.com",
    "www.notion.so",
    "app.notion.com",
    "api.openai.com",
    "openai.com",
    "chatgpt.com",
    "api.anthropic.com",
    "anthropic.com",
    "claude.ai",
];

const DASHBOARD_HTML: &str = include_str!("../index.html");

#[derive(Parser)]
#[command(name = "litellm-relay")]
#[command(about = "Local LiteLLM Gateway relay for AI app traffic")]
struct Cli {
    #[command(subcommand)]
    command: Option<CommandKind>,
}

#[derive(Subcommand)]
enum CommandKind {
    /// Run the local Relay proxy.
    Serve,
    /// Print the PAC file served by Relay.
    Pac,
    /// Create the local CA and print its certificate path.
    CaPath,
    /// Configure Gateway URL and API key for Relay ingest.
    Setup {
        #[arg(long)]
        gateway_url: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
    },
}

#[derive(Clone, Debug)]
struct RelayConfig {
    host: String,
    port: u16,
    log_path: PathBuf,
    notion_domains: Vec<String>,
    ai_domains: Vec<String>,
    shadow_enabled: bool,
    gateway_url: String,
    gateway_api_key: Option<String>,
    shadow_model: String,
    shadow_min_interval_seconds: u64,
    request_timeout_seconds: f64,
    payload_preview_bytes: usize,
    payload_body_bytes: usize,
    mitm_enabled: bool,
    mitm_ca_dir: PathBuf,
}

impl RelayConfig {
    fn from_env() -> Self {
        let home = home_dir();
        let relay_home = home.join(".litellm-relay");
        Self {
            host: env::var("LITELLM_RELAY_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            port: env::var("LITELLM_RELAY_PORT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(4142),
            log_path: env::var("LITELLM_RELAY_LOG_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| relay_home.join("relay.log.jsonl")),
            notion_domains: parse_domains(
                env::var("LITELLM_RELAY_NOTION_DOMAINS").ok(),
                DEFAULT_NOTION_DOMAINS,
            ),
            ai_domains: parse_domains(
                env::var("LITELLM_RELAY_AI_DOMAINS").ok(),
                DEFAULT_AI_DOMAINS,
            ),
            shadow_enabled: env_bool("LITELLM_RELAY_SHADOW_ENABLED", false),
            gateway_url: env::var("LITELLM_GATEWAY_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:4000".into()),
            gateway_api_key: env::var("LITELLM_GATEWAY_API_KEY")
                .ok()
                .filter(|v| !v.is_empty())
                .or_else(|| env::var("LITELLM_API_KEY").ok().filter(|v| !v.is_empty())),
            shadow_model: env::var("LITELLM_RELAY_SHADOW_MODEL")
                .unwrap_or_else(|_| "gpt-4o-mini".into()),
            shadow_min_interval_seconds: env::var("LITELLM_RELAY_SHADOW_MIN_INTERVAL_SECONDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(60),
            request_timeout_seconds: env::var("LITELLM_RELAY_REQUEST_TIMEOUT_SECONDS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(10.0),
            payload_preview_bytes: env::var("LITELLM_RELAY_PAYLOAD_PREVIEW_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(8192),
            payload_body_bytes: env::var("LITELLM_RELAY_PAYLOAD_BODY_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(262_144),
            mitm_enabled: env_bool("LITELLM_RELAY_CAPTURE_PAYLOADS", true),
            mitm_ca_dir: env::var("LITELLM_RELAY_MITM_CA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| relay_home.join("mitm")),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = RelayConfig::from_env();
    match cli.command.unwrap_or(CommandKind::Serve) {
        CommandKind::Serve => RelayProxy::new(config).serve_forever().await,
        CommandKind::Pac => {
            print!("{}", build_pac(&config));
            Ok(())
        }
        CommandKind::CaPath => {
            let ca = ensure_ca(&config.mitm_ca_dir)?;
            println!("{}", ca.cert_path.display());
            Ok(())
        }
        CommandKind::Setup {
            gateway_url,
            api_key,
        } => run_setup(gateway_url, api_key),
    }
}

fn run_setup(gateway_url: Option<String>, api_key: Option<String>) -> Result<()> {
    let gateway_url =
        gateway_url.unwrap_or_else(|| prompt("LiteLLM Gateway URL", "http://127.0.0.1:4000"));
    let login_url = format!(
        "{}/ui/login?redirect_to=/ui/?login=success&page=api-keys",
        gateway_url.trim_end_matches('/')
    );
    println!("Opening LiteLLM Gateway login/API key page:");
    println!("{login_url}");
    let _ = Command::new("open").arg(&login_url).status();

    let api_key = api_key.unwrap_or_else(|| prompt("Paste Relay Gateway key", ""));
    if api_key.trim().is_empty() {
        return Err(anyhow!("setup requires a LiteLLM Gateway API key"));
    }

    let relay_home = home_dir().join(".litellm-relay");
    fs::create_dir_all(&relay_home)?;
    let env_path = relay_home.join("env");
    let contents = format!(
        "LITELLM_RELAY_HOST=127.0.0.1\n\
         LITELLM_RELAY_PORT=4142\n\
         LITELLM_RELAY_LOG_PATH={}/relay.log.jsonl\n\
         LITELLM_GATEWAY_URL={}\n\
         LITELLM_GATEWAY_API_KEY={}\n\
         LITELLM_RELAY_SHADOW_ENABLED=0\n\
         LITELLM_RELAY_CAPTURE_PAYLOADS=1\n\
         LITELLM_RELAY_MITM_CA_DIR={}/mitm\n",
        relay_home.display(),
        gateway_url.trim_end_matches('/'),
        api_key.trim(),
        relay_home.display()
    );
    fs::write(&env_path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&env_path, fs::Permissions::from_mode(0o600))?;
    }
    println!("Wrote {}", env_path.display());
    Ok(())
}

fn prompt(label: &str, default: &str) -> String {
    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    let value = line.trim();
    if value.is_empty() {
        default.to_string()
    } else {
        value.to_string()
    }
}

struct RelayProxy {
    config: Arc<RelayConfig>,
    http_client: reqwest::Client,
    last_shadow_by_host: Mutex<HashMap<String, Instant>>,
}

impl RelayProxy {
    fn new(config: RelayConfig) -> Self {
        let timeout = Duration::from_secs_f64(config.request_timeout_seconds);
        let http_client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client configuration should be valid");
        Self {
            config: Arc::new(config),
            http_client,
            last_shadow_by_host: Mutex::new(HashMap::new()),
        }
    }

    async fn serve_forever(self) -> Result<()> {
        if let Some(parent) = self.config.log_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let listen = format!("{}:{}", self.config.host, self.config.port);
        let listener = TcpListener::bind(&listen).await?;
        self.log_event(json!({
            "event": "relay_started",
            "listen": listen,
            "capture_payloads": self.config.mitm_enabled,
            "shadow_enabled": self.config.shadow_enabled,
            "runtime": "rust",
        }))?;

        let proxy = Arc::new(self);
        loop {
            let (stream, peer) = listener.accept().await?;
            let proxy = Arc::clone(&proxy);
            tokio::spawn(async move {
                if let Err(error) = proxy.handle_client(stream, peer).await {
                    let _ = proxy.log_event(json!({
                        "event": "client_error",
                        "peer": peer.to_string(),
                        "error": error.to_string(),
                    }));
                }
            });
        }
    }

    async fn handle_client(&self, mut stream: TcpStream, peer: SocketAddr) -> Result<()> {
        let header = match read_until_headers(&mut stream).await {
            Ok(header) => header,
            Err(_) => return Ok(()),
        };
        let header_text = String::from_utf8_lossy(&header).to_string();
        let (method, target) = parse_start_line(&header_text)?;
        let route = parse_route(&target);

        if matches!(method.as_str(), "GET" | "HEAD")
            && matches!(route.path.as_str(), "/" | "/index.html")
        {
            return write_response(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                DASHBOARD_HTML.as_bytes(),
                method == "GET",
            )
            .await;
        }

        if matches!(method.as_str(), "GET" | "HEAD")
            && matches!(route.path.as_str(), "/proxy.pac" | "/pac")
        {
            let pac = build_pac(&self.config);
            return write_response(
                &mut stream,
                200,
                "application/x-ns-proxy-autoconfig",
                pac.as_bytes(),
                method == "GET",
            )
            .await;
        }

        if method == "GET" && route.path == "/api/status" {
            return self.write_json(&mut stream, self.status_payload()?).await;
        }

        if method == "GET" && route.path == "/api/events" {
            let limit = parse_limit(&route.query);
            return self
                .write_json(
                    &mut stream,
                    json!({
                        "events": read_events(&self.config.log_path, limit),
                        "limit": limit,
                    }),
                )
                .await;
        }

        if method == "POST" && route.path == "/api/events/clear" {
            if let Some(parent) = self.config.log_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&self.config.log_path, "")?;
            self.log_event(json!({
                "event": "relay_log_cleared",
                "listen": format!("{}:{}", self.config.host, self.config.port),
            }))?;
            return self.write_json(&mut stream, json!({"ok": true})).await;
        }

        if method == "CONNECT" {
            return self.handle_connect(stream, target, peer).await;
        }

        write_response(
            &mut stream,
            501,
            "text/plain",
            b"litellm-relay only supports CONNECT tunneling and local dashboard endpoints\n",
            true,
        )
        .await
    }

    async fn handle_connect(
        &self,
        mut client: TcpStream,
        target: String,
        peer: SocketAddr,
    ) -> Result<()> {
        let (host, port) = parse_connect_target(&target)?;
        let started_at = Instant::now();
        let event_id = Uuid::new_v4().to_string();
        let app = classify_host(&host, &self.config);
        let ai_match = is_ai_host(&host, &self.config);
        let notion_match = is_notion_host(&host, &self.config);
        let mut event = json!({
            "event_id": event_id,
            "event": "connect",
            "method": "CONNECT",
            "host": host,
            "port": port,
            "peer": peer.to_string(),
            "app": app,
            "ai_match": ai_match,
            "notion_match": notion_match,
        });

        if ai_match {
            let shadow = self.maybe_shadow(&event).await;
            event["shadow"] = shadow;
        }
        self.log_event(event)?;

        if self.config.mitm_enabled && ai_match {
            return self
                .handle_mitm_connect(client, host, port, event_id, started_at)
                .await;
        }

        let mut upstream = match TcpStream::connect((host.as_str(), port)).await {
            Ok(stream) => stream,
            Err(error) => {
                self.log_event(json!({
                    "event": "connect_failed",
                    "host": host,
                    "port": port,
                    "error": error.kind().to_string(),
                }))?;
                return write_response(
                    &mut client,
                    502,
                    "text/plain",
                    b"upstream connect failed\n",
                    true,
                )
                .await;
            }
        };
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        let (bytes_out, bytes_in) = copy_bidirectional_counted(&mut client, &mut upstream).await?;
        self.log_event(json!({
            "event_id": event_id,
            "event": "connect_closed",
            "method": "CONNECT",
            "host": host,
            "port": port,
            "app": app,
            "ai_match": ai_match,
            "notion_match": notion_match,
            "duration_ms": started_at.elapsed().as_millis() as u64,
            "bytes_out": bytes_out,
            "bytes_in": bytes_in,
        }))?;
        Ok(())
    }

    async fn handle_mitm_connect(
        &self,
        mut client: TcpStream,
        host: String,
        port: u16,
        event_id: String,
        started_at: Instant,
    ) -> Result<()> {
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        let server_config = match server_tls_config(&host, &self.config.mitm_ca_dir) {
            Ok(config) => config,
            Err(error) => {
                self.log_event(json!({
                    "event_id": event_id,
                    "event": "payload_capture_failed",
                    "host": host,
                    "method": "CONNECT",
                    "error": error.to_string(),
                }))?;
                return Ok(());
            }
        };
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let mut client_tls = match acceptor.accept(client).await {
            Ok(stream) => stream,
            Err(error) => {
                self.log_event(json!({
                    "event_id": event_id,
                    "event": "payload_capture_failed",
                    "host": host,
                    "method": "CONNECT",
                    "error": error.to_string(),
                }))?;
                return Ok(());
            }
        };

        let upstream_tcp = match TcpStream::connect((host.as_str(), port)).await {
            Ok(stream) => stream,
            Err(error) => {
                self.log_event(json!({
                    "event_id": event_id,
                    "event": "connect_failed",
                    "host": host,
                    "port": port,
                    "error": error.kind().to_string(),
                }))?;
                return Ok(());
            }
        };
        let connector = TlsConnector::from(Arc::new(client_tls_config()));
        let server_name =
            ServerName::try_from(host.clone()).context("invalid upstream DNS name")?;
        let mut upstream_tls = connector.connect(server_name, upstream_tcp).await?;

        let mut bytes_out = 0usize;
        let mut bytes_in = 0usize;
        loop {
            let request = match read_http_message(&mut client_tls).await {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(error) => {
                    self.log_event(json!({
                        "event_id": event_id,
                        "event": "payload_capture_closed",
                        "host": host,
                        "error": error.to_string(),
                    }))?;
                    break;
                }
            };
            bytes_out += request.raw.len();
            let capture_event_id = Uuid::new_v4().to_string();
            let request_started_at = Utc::now();
            let request_started = Instant::now();
            let request_line = parse_request_line(&request.header_text);
            let request_payload = build_http_payload(
                &request.body,
                &request.headers,
                self.config.payload_preview_bytes,
                self.config.payload_body_bytes,
                json!({
                    "method": request_line.method,
                    "path": request_line.path,
                    "headers": redact_headers(&request.headers),
                }),
            );
            self.log_event(json!({
                "event_id": capture_event_id,
                "connection_event_id": event_id,
                "event": "http_request",
                "method": request_line.method,
                "path": request_line.path,
                "host": host,
                "app": classify_host(&host, &self.config),
                "ai_match": is_ai_host(&host, &self.config),
                "notion_match": is_notion_host(&host, &self.config),
                "headers": redact_headers(&request.headers),
                "request_bytes": request.body.len(),
                "request_preview": request_payload.get("body_preview").cloned().unwrap_or(Value::String(String::new())),
                "request_truncated": request_payload.get("preview_truncated").cloned().unwrap_or(Value::Bool(false)),
            }))?;

            upstream_tls.write_all(&request.raw).await?;
            upstream_tls.flush().await?;

            let response = match read_http_message(&mut upstream_tls).await? {
                Some(message) => message,
                None => break,
            };
            bytes_in += response.raw.len();
            let status_code = parse_response_status(&response.header_text);
            let response_payload = build_http_payload(
                &response.body,
                &response.headers,
                self.config.payload_preview_bytes,
                self.config.payload_body_bytes,
                json!({
                    "status_code": status_code,
                    "headers": redact_headers(&response.headers),
                }),
            );
            self.log_event(json!({
                "event_id": capture_event_id,
                "connection_event_id": event_id,
                "event": "http_response",
                "method": request_line.method,
                "path": request_line.path,
                "host": host,
                "app": classify_host(&host, &self.config),
                "status_code": status_code,
                "headers": redact_headers(&response.headers),
                "response_bytes": response.body.len(),
                "response_preview": response_payload.get("body_preview").cloned().unwrap_or(Value::String(String::new())),
                "response_truncated": response_payload.get("preview_truncated").cloned().unwrap_or(Value::Bool(false)),
            }))?;

            let ended_at = Utc::now();
            let ingest = self
                .ingest_capture(CaptureIngest {
                    event_id: capture_event_id.clone(),
                    host: host.clone(),
                    app: classify_host(&host, &self.config),
                    method: request_line.method.clone(),
                    path: request_line.path.clone(),
                    started_at: request_started_at,
                    ended_at,
                    request_payload,
                    response_payload,
                    duration_ms: request_started.elapsed().as_millis() as u64,
                })
                .await;
            self.log_event(json!({
                "event_id": capture_event_id,
                "connection_event_id": event_id,
                "event": "gateway_ingest",
                "host": host,
                "app": classify_host(&host, &self.config),
                "path": request_line.path,
                "attempted": ingest.attempted,
                "ok": ingest.ok,
                "status": ingest.status,
                "error": ingest.error,
            }))?;

            client_tls.write_all(&response.raw).await?;
            client_tls.flush().await?;

            if should_close(&request.headers) || should_close(&response.headers) {
                break;
            }
        }

        let _ = upstream_tls.shutdown().await;
        let _ = client_tls.shutdown().await;
        self.log_event(json!({
            "event_id": event_id,
            "event": "connect_closed",
            "method": "CONNECT",
            "host": host,
            "port": port,
            "app": classify_host(&host, &self.config),
            "ai_match": is_ai_host(&host, &self.config),
            "notion_match": is_notion_host(&host, &self.config),
            "capture_payloads": true,
            "duration_ms": started_at.elapsed().as_millis() as u64,
            "bytes_out": bytes_out,
            "bytes_in": bytes_in,
        }))?;
        Ok(())
    }

    async fn maybe_shadow(&self, event: &Value) -> Value {
        if !self.config.shadow_enabled {
            return json!({"attempted": false, "ok": false});
        }
        let Some(api_key) = &self.config.gateway_api_key else {
            return json!({"attempted": false, "ok": false, "error": "LITELLM_GATEWAY_API_KEY is not set"});
        };
        let host = event
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let now = Instant::now();
        {
            let mut shadows = self.last_shadow_by_host.lock().await;
            if let Some(last_shadow) = shadows.get(host) {
                if now.duration_since(*last_shadow)
                    < Duration::from_secs(self.config.shadow_min_interval_seconds)
                {
                    return json!({"attempted": false, "ok": false, "error": "throttled"});
                }
            }
            shadows.insert(host.to_string(), now);
        }
        let event_id = event
            .get("event_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let payload = build_shadow_payload(event, &self.config, &event_id);
        let response = self
            .http_client
            .post(format!(
                "{}/v1/chat/completions",
                self.config.gateway_url.trim_end_matches('/')
            ))
            .bearer_auth(api_key)
            .json(&payload)
            .send()
            .await;
        match response {
            Ok(response) => json!({
                "attempted": true,
                "ok": response.status().is_success(),
                "status": response.status().as_u16(),
                "event_id": event_id,
            }),
            Err(error) => json!({
                "attempted": true,
                "ok": false,
                "error": error.to_string(),
                "event_id": event_id,
            }),
        }
    }

    async fn ingest_capture(&self, capture: CaptureIngest) -> IngestResult {
        let Some(api_key) = &self.config.gateway_api_key else {
            return IngestResult {
                attempted: false,
                ok: false,
                status: None,
                error: Some("LITELLM_GATEWAY_API_KEY is not set".into()),
            };
        };
        let status_code = capture
            .response_payload
            .get("status_code")
            .and_then(Value::as_u64);
        let status = if status_code.is_some_and(|code| code >= 400) {
            "failure"
        } else {
            "success"
        };
        let payload = json!({
            "logs": [{
                "request_id": format!("relay-{}", capture.event_id),
                "call_type": "relay_capture",
                "model": format!("{}-ai", if capture.app.is_empty() { "local-ai" } else { &capture.app }),
                "api_base": format!("https://{}", capture.host),
                "spend": 0,
                "total_tokens": 0,
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "startTime": capture.started_at.to_rfc3339(),
                "endTime": capture.ended_at.to_rfc3339(),
                "request_duration_ms": capture.duration_ms,
                "status": status,
                "request_tags": ["litellm-relay", capture.app.as_str()],
                "metadata": {
                    "source": "litellm-relay",
                    "runtime": "rust",
                    "app": capture.app,
                    "host": capture.host,
                    "method": capture.method,
                    "path": capture.path,
                    "status_code": status_code,
                    "device_id": hostname(),
                    "local_user": env::var("USER").unwrap_or_default(),
                    "relay_event_id": capture.event_id,
                },
                "proxy_server_request": capture.request_payload,
                "response": capture.response_payload,
            }]
        });
        match self
            .http_client
            .post(format!(
                "{}/internal/collector/events",
                self.config.gateway_url.trim_end_matches('/')
            ))
            .bearer_auth(api_key)
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => IngestResult {
                attempted: true,
                ok: response.status().is_success(),
                status: Some(response.status().as_u16()),
                error: None,
            },
            Err(error) => IngestResult {
                attempted: true,
                ok: false,
                status: None,
                error: Some(error.to_string()),
            },
        }
    }

    async fn write_json(&self, stream: &mut TcpStream, payload: Value) -> Result<()> {
        let body = serde_json::to_vec(&payload)?;
        write_response(stream, 200, "application/json; charset=utf-8", &body, true).await
    }

    fn status_payload(&self) -> Result<Value> {
        let ca_path = if self.config.mitm_enabled {
            Some(
                ensure_ca(&self.config.mitm_ca_dir)?
                    .cert_path
                    .display()
                    .to_string(),
            )
        } else {
            None
        };
        Ok(json!({
            "listen": format!("{}:{}", self.config.host, self.config.port),
            "log_path": self.config.log_path.display().to_string(),
            "ai_domains": self.config.ai_domains,
            "notion_domains": self.config.notion_domains,
            "capture_payloads": self.config.mitm_enabled,
            "mitm_ca_path": ca_path,
            "shadow_enabled": self.config.shadow_enabled,
            "gateway_url": self.config.gateway_url,
            "events_loaded": read_events(&self.config.log_path, 1000).len(),
            "runtime": "rust",
        }))
    }

    fn log_event(&self, event: Value) -> Result<()> {
        if let Some(parent) = self.config.log_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let event = redact_event(event);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.config.log_path)?;
        writeln!(file, "{}", serde_json::to_string(&event)?)?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
struct IngestResult {
    attempted: bool,
    ok: bool,
    status: Option<u16>,
    error: Option<String>,
}

#[derive(Debug)]
struct CaptureIngest {
    event_id: String,
    host: String,
    app: String,
    method: String,
    path: String,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    request_payload: Value,
    response_payload: Value,
    duration_ms: u64,
}

#[derive(Debug)]
struct Route {
    path: String,
    query: String,
}

#[derive(Debug)]
struct HttpMessage {
    raw: Vec<u8>,
    header_text: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Debug)]
struct RequestLine {
    method: String,
    path: String,
}

#[derive(Debug)]
struct CertificateAuthority {
    cert_path: PathBuf,
    key_path: PathBuf,
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    include_body: bool,
) -> Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        _ => "OK",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-length: {}\r\ncontent-type: {content_type}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    if include_body {
        stream.write_all(body).await?;
    }
    stream.shutdown().await?;
    Ok(())
}

async fn read_until_headers<T>(stream: &mut T) -> Result<Vec<u8>>
where
    T: AsyncRead + Unpin,
{
    let mut out = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    loop {
        let read = stream.read(&mut byte).await?;
        if read == 0 {
            if out.is_empty() {
                return Err(anyhow!("eof"));
            }
            break;
        }
        out.push(byte[0]);
        if out.ends_with(b"\r\n\r\n") {
            break;
        }
        if out.len() > 1024 * 1024 {
            return Err(anyhow!("headers exceeded 1 MiB"));
        }
    }
    Ok(out)
}

async fn read_http_message<T>(stream: &mut T) -> Result<Option<HttpMessage>>
where
    T: AsyncRead + Unpin,
{
    let header = match read_until_headers(stream).await {
        Ok(header) => header,
        Err(_) => return Ok(None),
    };
    let header_text = String::from_utf8_lossy(&header).to_string();
    let headers = parse_headers(&header_text);
    let mut raw_body = Vec::new();
    let mut body = Vec::new();
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.to_ascii_lowercase().contains("chunked"))
    {
        let (raw, decoded) = read_chunked_body(stream).await?;
        raw_body = raw;
        body = decoded;
    } else if let Some(content_length) = headers.get("content-length") {
        let content_length = content_length
            .parse::<usize>()
            .context("invalid content-length")?;
        body.resize(content_length, 0);
        stream.read_exact(&mut body).await?;
        raw_body = body.clone();
    }
    let mut raw = header;
    raw.extend_from_slice(&raw_body);
    Ok(Some(HttpMessage {
        raw,
        header_text,
        headers,
        body,
    }))
}

async fn read_chunked_body<T>(stream: &mut T) -> Result<(Vec<u8>, Vec<u8>)>
where
    T: AsyncRead + Unpin,
{
    let mut raw = Vec::new();
    let mut decoded = Vec::new();
    loop {
        let size_line = read_line_crlf(stream).await?;
        raw.extend_from_slice(&size_line);
        let size_raw = String::from_utf8_lossy(&size_line);
        let size_hex = size_raw.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).unwrap_or(0);
        if size == 0 {
            let trailer = read_line_crlf(stream).await?;
            raw.extend_from_slice(&trailer);
            break;
        }
        let mut chunk = vec![0u8; size + 2];
        stream.read_exact(&mut chunk).await?;
        decoded.extend_from_slice(&chunk[..size]);
        raw.extend_from_slice(&chunk);
    }
    Ok((raw, decoded))
}

async fn read_line_crlf<T>(stream: &mut T) -> Result<Vec<u8>>
where
    T: AsyncRead + Unpin,
{
    let mut out = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = stream.read(&mut byte).await?;
        if read == 0 {
            break;
        }
        out.push(byte[0]);
        if out.ends_with(b"\r\n") {
            break;
        }
    }
    Ok(out)
}

async fn copy_bidirectional_counted(
    left: &mut TcpStream,
    right: &mut TcpStream,
) -> Result<(u64, u64)> {
    let result = tokio::io::copy_bidirectional(left, right).await?;
    Ok(result)
}

fn parse_start_line(header_text: &str) -> Result<(String, String)> {
    let first = header_text
        .lines()
        .next()
        .ok_or_else(|| anyhow!("missing request line"))?;
    let mut parts = first.splitn(3, ' ');
    let method = parts
        .next()
        .ok_or_else(|| anyhow!("missing method"))?
        .to_ascii_uppercase();
    let target = parts
        .next()
        .ok_or_else(|| anyhow!("missing target"))?
        .to_string();
    Ok((method, target))
}

fn parse_route(target: &str) -> Route {
    let parsed = url::Url::parse(&format!("http://relay.local{target}")).ok();
    Route {
        path: parsed
            .as_ref()
            .map(|url| url.path().to_string())
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| "/".into()),
        query: parsed
            .as_ref()
            .and_then(|url| url.query())
            .unwrap_or("")
            .to_string(),
    }
}

fn parse_limit(query: &str) -> usize {
    query
        .split('&')
        .find_map(|part| part.strip_prefix("limit="))
        .and_then(|raw| raw.parse::<usize>().ok())
        .map(|value| value.clamp(1, 1000))
        .unwrap_or(250)
}

fn parse_connect_target(target: &str) -> Result<(String, u16)> {
    if let Some(rest) = target.strip_prefix('[') {
        let (host, rest) = rest
            .split_once(']')
            .ok_or_else(|| anyhow!("invalid IPv6 target"))?;
        let port = rest.trim_start_matches(':').parse::<u16>().unwrap_or(443);
        return Ok((host.to_string(), port));
    }
    let (host, port) = target
        .split_once(':')
        .map(|(host, port)| (host, port.parse::<u16>().unwrap_or(443)))
        .unwrap_or((target, 443));
    Ok((normalize_host(host), port))
}

fn parse_request_line(header_text: &str) -> RequestLine {
    let first = header_text.lines().next().unwrap_or_default();
    let mut parts = first.splitn(3, ' ');
    RequestLine {
        method: parts.next().unwrap_or("UNKNOWN").to_string(),
        path: parts.next().unwrap_or("").to_string(),
    }
}

fn parse_response_status(header_text: &str) -> Option<u16> {
    header_text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
}

fn parse_headers(header_text: &str) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    for line in header_text.split("\r\n").skip(1) {
        if line.is_empty() {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    headers
}

fn should_close(headers: &HashMap<String, String>) -> bool {
    headers
        .get("connection")
        .is_some_and(|value| value.eq_ignore_ascii_case("close"))
}

fn redact_headers(headers: &HashMap<String, String>) -> Value {
    let sensitive = [
        "authorization",
        "cookie",
        "set-cookie",
        "x-notion-token",
        "x-api-key",
    ];
    let mut redacted = Map::new();
    for (key, value) in headers {
        let value = if sensitive.contains(&key.as_str()) {
            "<redacted>"
        } else {
            value
        };
        redacted.insert(key.clone(), Value::String(value.to_string()));
    }
    Value::Object(redacted)
}

fn build_http_payload(
    body: &[u8],
    headers: &HashMap<String, String>,
    preview_limit: usize,
    body_limit: usize,
    extra: Value,
) -> Value {
    let mut payload = match extra {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    payload.insert("body_bytes".into(), json!(body.len()));
    match decode_content_body(body, headers) {
        Some(decoded) => {
            payload.insert(
                "body_preview".into(),
                json!(preview_bytes(&decoded, preview_limit)),
            );
            payload.insert("body".into(), json!(preview_bytes(&decoded, body_limit)));
            payload.insert("decoded_body_bytes".into(), json!(decoded.len()));
            payload.insert("body_truncated".into(), json!(decoded.len() > body_limit));
            payload.insert(
                "preview_truncated".into(),
                json!(decoded.len() > preview_limit),
            );
            payload.insert("truncated".into(), json!(decoded.len() > preview_limit));
        }
        None => {
            let encoding = headers
                .get("content-encoding")
                .map(String::as_str)
                .unwrap_or("binary");
            payload.insert(
                "body_preview".into(),
                json!(format!(
                    "[{encoding} encoded body unavailable as text; {} bytes captured]",
                    body.len()
                )),
            );
            payload.insert("body".into(), Value::Null);
            payload.insert(
                "body_unavailable_reason".into(),
                json!(format!("{encoding} encoded body unavailable as text")),
            );
            payload.insert("body_truncated".into(), json!(false));
            payload.insert("preview_truncated".into(), json!(false));
            payload.insert("truncated".into(), json!(false));
        }
    }
    Value::Object(payload)
}

fn decode_content_body(body: &[u8], headers: &HashMap<String, String>) -> Option<Vec<u8>> {
    let encoding = headers
        .get("content-encoding")
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    match encoding.as_str() {
        "" => Some(body.to_vec()),
        "gzip" => {
            let mut decoder = flate2::read::GzDecoder::new(Cursor::new(body));
            let mut out = Vec::new();
            decoder.read_to_end(&mut out).ok().map(|_| out)
        }
        "deflate" => {
            let mut decoder = flate2::read::ZlibDecoder::new(Cursor::new(body));
            let mut out = Vec::new();
            decoder.read_to_end(&mut out).ok().map(|_| out)
        }
        "br" => {
            let mut decoder = brotli::Decompressor::new(Cursor::new(body), 4096);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out).ok().map(|_| out)
        }
        "zstd" => zstd::stream::decode_all(Cursor::new(body)).ok(),
        _ => Some(body.to_vec()),
    }
}

fn preview_bytes(body: &[u8], limit: usize) -> String {
    String::from_utf8_lossy(&body[..body.len().min(limit)]).replace('\0', "")
}

fn read_events(log_path: &Path, limit: usize) -> Vec<Value> {
    let Ok(contents) = fs::read_to_string(log_path) else {
        return vec![];
    };
    let mut events = VecDeque::with_capacity(limit);
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(line) {
            if events.len() == limit {
                events.pop_front();
            }
            events.push_back(Value::Object(map));
        }
    }
    events.into_iter().collect()
}

fn redact_event(mut event: Value) -> Value {
    if let Value::Object(map) = &mut event {
        for key in [
            "authorization",
            "cookie",
            "token_v2",
            "prompt",
            "body",
            "url",
        ] {
            map.remove(key);
        }
    }
    event
}

fn build_shadow_payload(event: &Value, config: &RelayConfig, event_id: &str) -> Value {
    let host = event
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut hasher = Sha256::new();
    hasher.update(host.as_bytes());
    let host_hash = format!("{:x}", hasher.finalize());
    let app = event.get("app").and_then(Value::as_str).unwrap_or("ai");
    let method = event
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("CONNECT");
    json!({
        "model": config.shadow_model,
        "messages": [
            {
                "role": "system",
                "content": "You confirm receipt of redacted LiteLLM Relay shadow events.",
            },
            {
                "role": "user",
                "content": format!(
                    "Return exactly OK. source={app} method={method} event_id={event_id} host_hash={}",
                    &host_hash[..16]
                ),
            },
        ],
        "metadata": {
            "source": "litellm-relay",
            "runtime": "rust",
            "shadow_source": app,
            "event_id": event_id,
            "host_hash": host_hash,
            "method": method,
            "timestamp": Utc::now().to_rfc3339(),
        },
    })
}

fn ensure_ca(ca_dir: &Path) -> Result<CertificateAuthority> {
    fs::create_dir_all(ca_dir)?;
    let cert_path = ca_dir.join("litellm-relay-ca.pem");
    let key_path = ca_dir.join("litellm-relay-ca-key.pem");
    if cert_path.exists() && key_path.exists() {
        return Ok(CertificateAuthority {
            cert_path,
            key_path,
        });
    }
    run_quiet(
        Command::new("openssl")
            .arg("req")
            .arg("-x509")
            .arg("-newkey")
            .arg("rsa:2048")
            .arg("-nodes")
            .arg("-sha256")
            .arg("-days")
            .arg("825")
            .arg("-keyout")
            .arg(&key_path)
            .arg("-out")
            .arg(&cert_path)
            .arg("-subj")
            .arg("/CN=LiteLLM Relay Local Root CA")
            .arg("-addext")
            .arg("basicConstraints=critical,CA:TRUE,pathlen:0")
            .arg("-addext")
            .arg("keyUsage=critical,keyCertSign,cRLSign"),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(CertificateAuthority {
        cert_path,
        key_path,
    })
}

fn ensure_leaf_cert(host: &str, ca_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let ca = ensure_ca(ca_dir)?;
    let certs_dir = ca_dir.join("certs");
    fs::create_dir_all(&certs_dir)?;
    let safe_host = safe_cert_name(host);
    let cert_path = certs_dir.join(format!("{safe_host}.pem"));
    let key_path = certs_dir.join(format!("{safe_host}-key.pem"));
    let csr_path = certs_dir.join(format!("{safe_host}.csr"));
    if cert_path.exists() && key_path.exists() {
        return Ok((cert_path, key_path));
    }
    let ext_path = certs_dir.join(format!("{safe_host}.ext"));
    fs::write(
        &ext_path,
        format!(
            "basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:{host}\n"
        ),
    )?;
    let req_result = run_quiet(
        Command::new("openssl")
            .arg("req")
            .arg("-newkey")
            .arg("rsa:2048")
            .arg("-nodes")
            .arg("-keyout")
            .arg(&key_path)
            .arg("-out")
            .arg(&csr_path)
            .arg("-subj")
            .arg(format!("/CN={host}")),
    );
    if let Err(error) = req_result {
        let _ = fs::remove_file(&csr_path);
        let _ = fs::remove_file(&ext_path);
        return Err(error);
    }
    let sign_result = run_quiet(
        Command::new("openssl")
            .arg("x509")
            .arg("-req")
            .arg("-in")
            .arg(&csr_path)
            .arg("-CA")
            .arg(&ca.cert_path)
            .arg("-CAkey")
            .arg(&ca.key_path)
            .arg("-CAcreateserial")
            .arg("-out")
            .arg(&cert_path)
            .arg("-days")
            .arg("90")
            .arg("-sha256")
            .arg("-extfile")
            .arg(&ext_path),
    );
    let _ = fs::remove_file(&csr_path);
    let _ = fs::remove_file(&ext_path);
    sign_result?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    }
    Ok((cert_path, key_path))
}

fn server_tls_config(host: &str, ca_dir: &Path) -> Result<ServerConfig> {
    let (cert_path, key_path) = ensure_leaf_cert(host, ca_dir)?;
    let cert_file = fs::read(&cert_path)?;
    let key_file = fs::read(&key_path)?;
    let certs = rustls_pemfile::certs(&mut Cursor::new(cert_file))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut Cursor::new(key_file))?
        .ok_or_else(|| anyhow!("leaf private key not found"))?;
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

fn client_tls_config() -> ClientConfig {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config
}

fn run_quiet(command: &mut Command) -> Result<()> {
    let status = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("command failed with status {status}"))
    }
}

fn safe_cert_name(host: &str) -> String {
    let cleaned = host
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(['.', '_'])
        .to_string();
    if cleaned.is_empty() {
        "host".into()
    } else {
        cleaned
    }
}

fn build_pac(config: &RelayConfig) -> String {
    let domains = config
        .ai_domains
        .iter()
        .map(|domain| format!("    \"{domain}\""))
        .collect::<Vec<_>>()
        .join(",\n");
    format!(
        "function FindProxyForURL(url, host) {{\n  var relayProxy = \"PROXY {}:{}\";\n  var notionDomains = [\n{}\n  ];\n\n  host = host.toLowerCase();\n  for (var i = 0; i < notionDomains.length; i++) {{\n    var domain = notionDomains[i];\n    if (host === domain || dnsDomainIs(host, \".\" + domain)) {{\n      return relayProxy;\n    }}\n  }}\n\n  return \"DIRECT\";\n}}\n",
        config.host, config.port, domains
    )
}

fn parse_domains(raw: Option<String>, default: &[&str]) -> Vec<String> {
    raw.map(|value| {
        value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
            .collect()
    })
    .filter(|domains: &Vec<String>| !domains.is_empty())
    .unwrap_or_else(|| default.iter().map(|value| value.to_string()).collect())
}

fn normalize_host(host: &str) -> String {
    let host = host.trim().to_ascii_lowercase();
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest).to_string();
    }
    host.split(':').next().unwrap_or(&host).to_string()
}

fn is_domain_match(host: &str, domains: &[String]) -> bool {
    let normalized = normalize_host(host);
    domains
        .iter()
        .any(|domain| normalized == *domain || normalized.ends_with(&format!(".{domain}")))
}

fn is_notion_host(host: &str, config: &RelayConfig) -> bool {
    is_domain_match(host, &config.notion_domains)
}

fn is_ai_host(host: &str, config: &RelayConfig) -> bool {
    is_domain_match(host, &config.ai_domains)
}

fn classify_host(host: &str, config: &RelayConfig) -> String {
    let normalized = normalize_host(host);
    let openai = ["api.openai.com", "openai.com", "chatgpt.com"]
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    let anthropic = ["api.anthropic.com", "anthropic.com", "claude.ai"]
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    if is_domain_match(&normalized, &config.notion_domains) {
        "notion".into()
    } else if is_domain_match(&normalized, &openai) {
        "openai".into()
    } else if is_domain_match(&normalized, &anthropic) {
        "anthropic".into()
    } else if is_ai_host(&normalized, config) {
        "ai".into()
    } else {
        "unknown".into()
    }
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn home_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_connect_target_defaults_to_tls_port() {
        assert_eq!(
            parse_connect_target("www.notion.so").unwrap(),
            ("www.notion.so".to_string(), 443)
        );
    }

    #[test]
    fn parse_limit_clamps_invalid_values() {
        assert_eq!(parse_limit("limit=2"), 2);
        assert_eq!(parse_limit("limit=999999"), 1000);
        assert_eq!(parse_limit("limit=nope"), 250);
    }

    #[test]
    fn redact_event_drops_sensitive_fields() {
        let redacted = redact_event(json!({
            "event": "connect",
            "host": "www.notion.so",
            "authorization": "Bearer secret",
            "cookie": "token_v2=secret",
            "body": "prompt",
        }));
        assert!(redacted.get("authorization").is_none());
        assert!(redacted.get("cookie").is_none());
        assert!(redacted.get("body").is_none());
        assert_eq!(redacted["host"], "www.notion.so");
    }

    #[test]
    fn safe_cert_name_removes_path_characters() {
        assert_eq!(safe_cert_name("www.notion.so:443"), "www.notion.so_443");
    }

    #[test]
    fn build_http_payload_includes_decoded_body() {
        let headers = HashMap::from([("content-type".to_string(), "application/json".to_string())]);
        let payload = build_http_payload(
            br#"{"prompt":"hello notion"}"#,
            &headers,
            10,
            100,
            json!({"method": "POST", "path": "/api/v3/runInferenceTranscript"}),
        );
        assert_eq!(payload["body"], "{\"prompt\":\"hello notion\"}");
        assert_eq!(payload["body_preview"], "{\"prompt\":");
        assert_eq!(payload["decoded_body_bytes"], 25);
        assert_eq!(payload["body_truncated"], false);
        assert_eq!(payload["preview_truncated"], true);
    }
}
