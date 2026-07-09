use std::{fs, net::SocketAddr, sync::Arc, time::Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use rustls::pki_types::ServerName;
use serde_json::{json, Value};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use uuid::Uuid;

use crate::{
    cert::{client_tls_config, ensure_ca, server_tls_config},
    config::{classify_host, is_ai_host, is_notion_host, RelayConfig},
    events::{append_event, clear_events, read_events},
    gateway::{CaptureIngest, GatewayClient, IngestResult},
    http::{
        build_http_payload, copy_bidirectional_counted, parse_connect_target, parse_limit,
        parse_request_line, parse_response_status, parse_route, parse_start_line,
        read_http_message, read_until_headers, redact_headers, should_close, write_response,
    },
    pac::build_pac,
};

const DASHBOARD_HTML: &str = include_str!("static/dashboard.html");

pub struct RelayProxy {
    config: Arc<RelayConfig>,
    gateway: GatewayClient,
}

impl RelayProxy {
    pub fn new(config: RelayConfig) -> Self {
        let config = Arc::new(config);
        let gateway = GatewayClient::new(Arc::clone(&config));
        Self { config, gateway }
    }

    pub async fn serve_forever(self) -> Result<()> {
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
            clear_events(&self.config.log_path)?;
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
        let target = parse_connect_target(&target)?;
        let started_at = Instant::now();
        let event_id = Uuid::new_v4().to_string();
        let app = classify_host(&target.host, &self.config);
        let ai_match = is_ai_host(&target.host, &self.config);
        let notion_match = is_notion_host(&target.host, &self.config);
        let mut event = json!({
            "event_id": event_id,
            "event": "connect",
            "method": "CONNECT",
            "host": target.host,
            "port": target.port,
            "peer": peer.to_string(),
            "app": app,
            "ai_match": ai_match,
            "notion_match": notion_match,
        });

        if ai_match {
            event["shadow"] = self.gateway.maybe_shadow(&event).await;
        }
        self.log_event(event)?;

        if self.config.mitm_enabled && ai_match {
            return self
                .handle_mitm_connect(target.host, target.port, event_id, started_at, client)
                .await;
        }

        let mut upstream = match TcpStream::connect((target.host.as_str(), target.port)).await {
            Ok(stream) => stream,
            Err(error) => {
                self.log_event(json!({
                    "event": "connect_failed",
                    "host": target.host,
                    "port": target.port,
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
            "host": target.host,
            "port": target.port,
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
        host: String,
        port: u16,
        event_id: String,
        started_at: Instant,
        mut client: TcpStream,
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

            let ingest = self
                .gateway
                .ingest_capture(CaptureIngest {
                    event_id: capture_event_id.clone(),
                    host: host.clone(),
                    app: classify_host(&host, &self.config),
                    method: request_line.method.clone(),
                    path: request_line.path.clone(),
                    started_at: request_started_at,
                    ended_at: Utc::now(),
                    request_payload,
                    response_payload,
                    duration_ms: request_started.elapsed().as_millis() as u64,
                })
                .await;
            self.log_ingest(
                &capture_event_id,
                &event_id,
                &host,
                &request_line.path,
                ingest,
            )?;

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

    fn log_ingest(
        &self,
        capture_event_id: &str,
        connection_event_id: &str,
        host: &str,
        path: &str,
        ingest: IngestResult,
    ) -> Result<()> {
        self.log_event(json!({
            "event_id": capture_event_id,
            "connection_event_id": connection_event_id,
            "event": "gateway_ingest",
            "host": host,
            "app": classify_host(host, &self.config),
            "path": path,
            "attempted": ingest.attempted,
            "ok": ingest.ok,
            "status": ingest.status,
            "error": ingest.error,
        }))
    }

    fn log_event(&self, event: Value) -> Result<()> {
        append_event(&self.config.log_path, event)
    }
}
