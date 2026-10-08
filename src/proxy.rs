use std::{fs, io::ErrorKind, net::SocketAddr, sync::Arc, time::Instant};

use anyhow::{bail, Context, Result};
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
    account::AccountService,
    apps::{classify_app_attribution, known_apps, AppAttribution},
    broker::Broker,
    cert::{client_tls_config, ensure_ca, server_tls_config},
    config::{is_ai_host, is_notion_host, RelayConfig},
    events::{append_event, clear_events, read_events},
    gateway::{CaptureIngest, GatewayClient, IngestResult},
    http::{
        build_http_payload, build_metadata_only_http_payload, copy_bidirectional_counted,
        parse_connect_target, parse_limit, parse_request_line, parse_response_status, parse_route,
        parse_start_line, read_http_body, read_http_head, read_http_message, read_until_headers,
        redact_headers, request_protocol_decision, response_protocol_decision,
        scrub_request_target, should_close, write_response, ProtocolCompatibilityDecision,
    },
    inference::{forward, gateway_client, is_inference_target, Head},
    mcp::service::McpService,
    pac::build_pac,
    terminal::{print_runtime_panel, print_trace_event},
    traffic::{classify_captured_traffic, CapturedTraffic, TrafficClassification},
};

const DASHBOARD_INDEX_HTML: &str = include_str!("static/dashboard/index.html");
const DASHBOARD_CSS: &[u8] = include_bytes!("static/dashboard/assets/dashboard.css");
const DASHBOARD_JS: &[u8] = include_bytes!("static/dashboard/assets/dashboard.js");
const DASHBOARD_FAVICON: &[u8] =
    br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#171717"/><text x="16" y="21" text-anchor="middle" font-family="ui-monospace, monospace" font-size="11" font-weight="700" fill="#fafafa">LR</text></svg>"##;

pub struct RelayProxy {
    config: Arc<RelayConfig>,
    gateway: GatewayClient,
    broker: Option<Arc<Broker>>,
    mcp: Option<Arc<McpService>>,
    account: Option<Arc<AccountService>>,
    inference: reqwest::Client,
}

impl RelayProxy {
    pub fn new(config: RelayConfig) -> Self {
        let config = Arc::new(config);
        let gateway = GatewayClient::new(Arc::clone(&config));
        Self {
            config,
            gateway,
            broker: None,
            mcp: None,
            account: None,
            inference: gateway_client(),
        }
    }

    /// Reports the credential broker's state on `/api/status`; the broker
    /// itself answers only over its own socket, never on this port.
    pub fn with_broker(self, broker: Arc<Broker>) -> Self {
        Self {
            broker: Some(broker),
            ..self
        }
    }

    pub fn with_mcp(self, mcp: Arc<McpService>) -> Self {
        Self {
            mcp: Some(mcp),
            ..self
        }
    }

    pub fn with_account(self, account: Arc<AccountService>) -> Self {
        Self {
            account: Some(account),
            ..self
        }
    }

    pub async fn serve_forever(self) -> Result<()> {
        if let Some(parent) = self.config.log_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let listen = format!("{}:{}", self.config.host, self.config.port);
        let listener = match TcpListener::bind(&listen).await {
            Ok(listener) => listener,
            Err(error) if error.kind() == ErrorKind::AddrInUse => {
                bail!(
                    "Relay could not start because {listen} is already in use.\n\n\
                     Stop the old Relay process and try again:\n\
                       pkill -f 'litellm_relay.cli serve'\n\
                       relay\n\n\
                     Or edit ~/.litellm-relay/config.yaml and set:\n\
                       relay:\n\
                         port: {}",
                    self.config.port + 1
                );
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to bind Relay to {listen}"));
            }
        };
        self.log_event(json!({
            "event": "relay_started",
            "listen": listen,
            "capture_payloads": self.config.mitm_enabled,
            "shadow_enabled": self.config.shadow_enabled,
            "runtime": "rust",
        }))?;
        print_runtime_panel(&self.config);

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
                DASHBOARD_INDEX_HTML.as_bytes(),
                method == "GET",
            )
            .await;
        }

        if matches!(method.as_str(), "GET" | "HEAD") && route.path == "/assets/dashboard.css" {
            return write_response(
                &mut stream,
                200,
                "text/css; charset=utf-8",
                DASHBOARD_CSS,
                method == "GET",
            )
            .await;
        }

        if matches!(method.as_str(), "GET" | "HEAD") && route.path == "/assets/dashboard.js" {
            return write_response(
                &mut stream,
                200,
                "application/javascript; charset=utf-8",
                DASHBOARD_JS,
                method == "GET",
            )
            .await;
        }

        if matches!(method.as_str(), "GET" | "HEAD")
            && matches!(route.path.as_str(), "/favicon.ico" | "/favicon.svg")
        {
            return write_response(
                &mut stream,
                200,
                "image/svg+xml",
                DASHBOARD_FAVICON,
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
            let credential = self.gateway.credential_status().await.to_json();
            return self
                .write_json(&mut stream, self.status_payload(credential)?)
                .await;
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

        if is_inference_target(&target) {
            let _ = stream.set_nodelay(true);
            let head = Head::parse(&method, &target, &header_text);
            return forward(
                &mut stream,
                peer,
                head,
                self.broker.as_ref(),
                &self.inference,
            )
            .await;
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
        let attribution = classify_app_attribution(&target.host, &self.config.ai_domains);
        let ai_match = is_ai_host(&target.host, &self.config);
        let notion_match = is_notion_host(&target.host, &self.config);
        let mut event = event_with_attribution(
            json!({
                "event_id": event_id,
                "event": "connect",
                "method": "CONNECT",
                "host": target.host,
                "port": target.port,
                "peer": peer.to_string(),
                "ai_match": ai_match,
                "notion_match": notion_match,
            }),
            &attribution,
        );

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
        self.log_event(event_with_attribution(
            json!({
                "event_id": event_id,
                "event": "connect_closed",
                "method": "CONNECT",
                "host": target.host,
                "port": target.port,
                "ai_match": ai_match,
                "notion_match": notion_match,
                "duration_ms": started_at.elapsed().as_millis() as u64,
                "bytes_out": bytes_out,
                "bytes_in": bytes_in,
            }),
            &attribution,
        ))?;
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
            let request_path = scrub_request_target(&request_line.path);
            let request_protocol = request_protocol_decision(&request.headers);
            let request_payload = build_http_payload(
                &request.body,
                &request.headers,
                self.config.payload_preview_bytes,
                self.config.payload_body_bytes,
                json!({
                    "method": request_line.method,
                    "path": request_path,
                    "headers": redact_headers(&request.headers),
                }),
            );
            upstream_tls.write_all(&request.raw).await?;
            upstream_tls.flush().await?;

            let response_head = match read_http_head(&mut upstream_tls).await? {
                Some(head) => head,
                None => break,
            };
            bytes_in += response_head.raw_headers.len();
            let status_code = parse_response_status(&response_head.header_text);
            let response_protocol = response_protocol_decision(status_code, &response_head.headers);

            if request_protocol.is_metadata_only() || response_protocol.is_metadata_only() {
                let tunnel_decision = metadata_tunnel_decision(request_protocol, response_protocol);
                let response_payload = build_metadata_only_http_payload(
                    &response_head.headers,
                    json!({
                        "status_code": status_code,
                    }),
                    tunnel_decision,
                );
                let attribution = classify_app_attribution(&host, &self.config.ai_domains);
                let app = attribution.destination_app.clone();
                let classification = classify_captured_traffic(CapturedTraffic {
                    app: &app,
                    host: &host,
                    method: &request_line.method,
                    path: &request_path,
                    request_headers: &request.headers,
                    request_payload: &request_payload,
                    response_payload: &response_payload,
                });
                self.log_event(event_with_attribution(json!({
                    "event_id": capture_event_id,
                    "connection_event_id": event_id,
                    "event": "http_request",
                    "method": request_line.method,
                    "path": request_path,
                    "host": host,
                    "ai_match": is_ai_host(&host, &self.config),
                    "notion_match": is_notion_host(&host, &self.config),
                    "traffic_kind": classification.kind,
                    "traffic_reason": classification.reason,
                    "collector_eligible": false,
                    "capture_mode": "metadata_only_tunnel",
                    "protocol_compatibility_reason": tunnel_decision.reason.as_str(),
                    "headers": redact_headers(&request.headers),
                    "request_bytes": request.body.len(),
                    "request_preview": request_payload.get("body_preview").cloned().unwrap_or(Value::String(String::new())),
                    "request_truncated": request_payload.get("preview_truncated").cloned().unwrap_or(Value::Bool(false)),
                }), &attribution))?;
                self.log_event(event_with_attribution(json!({
                    "event_id": capture_event_id,
                    "connection_event_id": event_id,
                    "event": "http_response",
                    "method": request_line.method,
                    "path": request_path,
                    "host": host,
                    "traffic_kind": classification.kind,
                    "traffic_reason": classification.reason,
                    "collector_eligible": false,
                    "capture_mode": "metadata_only_tunnel",
                    "protocol_compatibility_reason": tunnel_decision.reason.as_str(),
                    "status_code": status_code,
                    "headers": redact_headers(&response_head.headers),
                    "response_bytes": 0,
                    "response_preview": response_payload.get("body_preview").cloned().unwrap_or(Value::String(String::new())),
                    "response_truncated": false,
                }), &attribution))?;
                self.log_collector_skipped(
                    &capture_event_id,
                    &event_id,
                    &host,
                    &request_path,
                    &attribution,
                    &classification,
                )?;
                client_tls.write_all(&response_head.raw_headers).await?;
                client_tls.flush().await?;
                let (tunnel_bytes_out, tunnel_bytes_in) =
                    copy_bidirectional_counted(&mut client_tls, &mut upstream_tls).await?;
                bytes_out += tunnel_bytes_out as usize;
                bytes_in += tunnel_bytes_in as usize;
                self.log_event(event_with_attribution(
                    json!({
                        "event_id": capture_event_id,
                        "connection_event_id": event_id,
                        "event": "protocol_tunnel_closed",
                        "method": request_line.method,
                        "path": request_path,
                        "host": host,
                        "duration_ms": request_started.elapsed().as_millis() as u64,
                        "bytes_out": tunnel_bytes_out,
                        "bytes_in": tunnel_bytes_in,
                        "capture_mode": "metadata_only_tunnel",
                        "protocol_compatibility_reason": tunnel_decision.reason.as_str(),
                    }),
                    &attribution,
                ))?;
                break;
            }

            let response_body = read_http_body(&mut upstream_tls, &response_head.headers).await?;
            bytes_in += response_body.raw.len();
            let mut response_raw = response_head.raw_headers;
            response_raw.extend_from_slice(&response_body.raw);
            let response_payload = build_http_payload(
                &response_body.decoded,
                &response_head.headers,
                self.config.payload_preview_bytes,
                self.config.payload_body_bytes,
                json!({
                    "status_code": status_code,
                    "headers": redact_headers(&response_head.headers),
                    "capture_mode": "buffered_capture",
                    "protocol_compatibility_reason": response_protocol.reason.as_str(),
                }),
            );
            let attribution = classify_app_attribution(&host, &self.config.ai_domains);
            let app = attribution.destination_app.clone();
            let classification = classify_captured_traffic(CapturedTraffic {
                app: &app,
                host: &host,
                method: &request_line.method,
                path: &request_path,
                request_headers: &request.headers,
                request_payload: &request_payload,
                response_payload: &response_payload,
            });
            self.log_event(event_with_attribution(json!({
                "event_id": capture_event_id,
                "connection_event_id": event_id,
                "event": "http_request",
                "method": request_line.method,
                "path": request_path,
                "host": host,
                "ai_match": is_ai_host(&host, &self.config),
                "notion_match": is_notion_host(&host, &self.config),
                "traffic_kind": classification.kind,
                "traffic_reason": classification.reason,
                "collector_eligible": classification.is_ai_request(),
                "capture_mode": "buffered_capture",
                "protocol_compatibility_reason": response_protocol.reason.as_str(),
                "headers": redact_headers(&request.headers),
                "request_bytes": request.body.len(),
                "request_preview": request_payload.get("body_preview").cloned().unwrap_or(Value::String(String::new())),
                "request_truncated": request_payload.get("preview_truncated").cloned().unwrap_or(Value::Bool(false)),
            }), &attribution))?;
            self.log_event(event_with_attribution(json!({
                "event_id": capture_event_id,
                "connection_event_id": event_id,
                "event": "http_response",
                "method": request_line.method,
                "path": request_path,
                "host": host,
                "traffic_kind": classification.kind,
                "traffic_reason": classification.reason,
                "collector_eligible": classification.is_ai_request(),
                "status_code": status_code,
                "capture_mode": "buffered_capture",
                "protocol_compatibility_reason": response_protocol.reason.as_str(),
                "headers": redact_headers(&response_head.headers),
                "response_bytes": response_body.decoded.len(),
                "response_preview": response_payload.get("body_preview").cloned().unwrap_or(Value::String(String::new())),
                "response_truncated": response_payload.get("preview_truncated").cloned().unwrap_or(Value::Bool(false)),
            }), &attribution))?;
            if classification.is_ai_request() {
                let ingest = self
                    .gateway
                    .ingest_capture(CaptureIngest {
                        event_id: capture_event_id.clone(),
                        host: host.clone(),
                        attribution: attribution.clone(),
                        method: request_line.method.clone(),
                        path: request_path.clone(),
                        started_at: request_started_at,
                        ended_at: Utc::now(),
                        request_payload,
                        response_payload,
                        duration_ms: request_started.elapsed().as_millis() as u64,
                        classification,
                    })
                    .await;
                self.log_collector_ingest(
                    &capture_event_id,
                    &event_id,
                    &host,
                    &request_path,
                    &attribution,
                    ingest,
                )?;
            } else {
                self.log_collector_skipped(
                    &capture_event_id,
                    &event_id,
                    &host,
                    &request_path,
                    &attribution,
                    &classification,
                )?;
            }

            client_tls.write_all(&response_raw).await?;
            client_tls.flush().await?;

            if should_close(&request.headers) || should_close(&response_head.headers) {
                break;
            }
        }

        let _ = upstream_tls.shutdown().await;
        let _ = client_tls.shutdown().await;
        let attribution = classify_app_attribution(&host, &self.config.ai_domains);
        self.log_event(event_with_attribution(
            json!({
                "event_id": event_id,
                "event": "connect_closed",
                "method": "CONNECT",
                "host": host,
                "port": port,
                "ai_match": is_ai_host(&host, &self.config),
                "notion_match": is_notion_host(&host, &self.config),
                "capture_payloads": true,
                "duration_ms": started_at.elapsed().as_millis() as u64,
                "bytes_out": bytes_out,
                "bytes_in": bytes_in,
            }),
            &attribution,
        ))?;
        Ok(())
    }

    async fn write_json(&self, stream: &mut TcpStream, payload: Value) -> Result<()> {
        let body = serde_json::to_vec(&payload)?;
        write_response(stream, 200, "application/json; charset=utf-8", &body, true).await
    }

    fn status_payload(&self, credential: Value) -> Result<Value> {
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
            "known_apps": known_apps(),
            "attribution": {
                "destination_field": "destination_app",
                "compatibility_field": "app",
                "process_identity_field": "process_identity",
                "process_lookup_status_field": "process_lookup_status",
                "sources": ["known_app_catalog", "configured_ai_domain", "unmatched"],
                "confidences": ["high", "medium", "none"],
            },
            "runtime": "rust",
            "credential": credential,
            "broker": self.broker.as_ref().map(|broker| broker.status()),
            "mcp": self.mcp.as_ref().map(|mcp| mcp.status()),
            "account": self.account.as_ref().map(|account| account.status()),
            "environments": self.account.as_ref().map(|account| account.environments()),
        }))
    }

    fn log_collector_ingest(
        &self,
        capture_event_id: &str,
        connection_event_id: &str,
        host: &str,
        path: &str,
        attribution: &AppAttribution,
        ingest: IngestResult,
    ) -> Result<()> {
        self.log_event(event_with_attribution(
            json!({
                "event_id": capture_event_id,
                "connection_event_id": connection_event_id,
                "event": "collector_spend_logs",
                "host": host,
                "path": path,
                "attempted": ingest.attempted,
                "ok": ingest.ok,
                "status": ingest.status,
                "error": ingest.error,
            }),
            attribution,
        ))
    }

    fn log_collector_skipped(
        &self,
        capture_event_id: &str,
        connection_event_id: &str,
        host: &str,
        path: &str,
        attribution: &AppAttribution,
        classification: &TrafficClassification,
    ) -> Result<()> {
        self.log_event(event_with_attribution(
            json!({
                "event_id": capture_event_id,
                "connection_event_id": connection_event_id,
                "event": "collector_skipped",
                "host": host,
                "path": path,
                "traffic_kind": classification.kind,
                "traffic_reason": classification.reason,
            }),
            attribution,
        ))
    }

    fn log_event(&self, event: Value) -> Result<()> {
        print_trace_event(&event);
        append_event(&self.config.log_path, event)
    }
}

fn metadata_tunnel_decision(
    request: ProtocolCompatibilityDecision,
    response: ProtocolCompatibilityDecision,
) -> ProtocolCompatibilityDecision {
    if request.is_metadata_only() {
        request
    } else {
        response
    }
}

fn event_with_attribution(mut event: Value, attribution: &AppAttribution) -> Value {
    if let Value::Object(map) = &mut event {
        map.insert(
            "app".into(),
            Value::String(attribution.destination_app.clone()),
        );
        map.insert(
            "destination_app".into(),
            Value::String(attribution.destination_app.clone()),
        );
        map.insert(
            "attribution_source".into(),
            serde_json::to_value(attribution.attribution_source)
                .expect("attribution source should serialize"),
        );
        map.insert(
            "attribution_confidence".into(),
            serde_json::to_value(attribution.attribution_confidence)
                .expect("attribution confidence should serialize"),
        );
        map.insert(
            "process_lookup_status".into(),
            serde_json::to_value(attribution.process_lookup_status)
                .expect("process lookup status should serialize"),
        );
        if let Some(process_identity) = &attribution.process_identity {
            map.insert(
                "process_identity".into(),
                Value::String(process_identity.clone()),
            );
        }
    }
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        broker::{
            test_support::{static_key_settings, Rig},
            Context, Reply, Request,
        },
        config::RelaySettings,
    };

    #[test]
    fn should_add_credential_to_status_payload_and_keep_every_existing_key() {
        let mut config = RelaySettings::default().to_config();
        config.mitm_enabled = false;
        config.log_path = std::env::temp_dir().join("relay-status-test-missing.log.jsonl");
        let proxy = RelayProxy::new(config);

        let payload = proxy
            .status_payload(json!({"state": "rejected"}))
            .expect("status payload should build");
        let mut keys: Vec<&str> = payload
            .as_object()
            .expect("status payload should be an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();

        assert_eq!(
            keys,
            [
                "account",
                "ai_domains",
                "attribution",
                "broker",
                "capture_payloads",
                "credential",
                "environments",
                "events_loaded",
                "gateway_url",
                "known_apps",
                "listen",
                "log_path",
                "mcp",
                "mitm_ca_path",
                "notion_domains",
                "runtime",
                "shadow_enabled",
            ]
        );
        assert_eq!(payload["credential"], json!({"state": "rejected"}));
        assert_eq!(payload["broker"], Value::Null);
        assert_eq!(payload["mcp"], Value::Null);
        assert_eq!(payload["account"], Value::Null);
        assert_eq!(payload["environments"], Value::Null);
    }

    #[test]
    fn should_report_the_account_and_the_environments_on_status_without_any_token() {
        use crate::{account::test_support::account_on, config::EnvironmentEntry};

        let mut settings = static_key_settings("sk-status-secret");
        settings.gateway.team = Some("team-a".to_string());
        settings.environments = vec![EnvironmentEntry {
            name: "dev".to_string(),
            url: settings.gateway.url.clone(),
            team: None,
        }];
        let mut config = settings.to_config();
        config.mitm_enabled = false;
        config.log_path = std::env::temp_dir().join("relay-status-test-missing.log.jsonl");
        let (_rig, http, account) = account_on(settings.clone());
        http.answer(
            "/user/info",
            200,
            r#"{"user_id":"u","user_info":{"user_email":"dev@example.com"},"teams":[{"team_id":"team-a","team_alias":"Team A"}]}"#,
        );
        http.answer(
            "/team/info?team_id=team-a",
            200,
            r#"{"team_id":"team-a","team_info":{"spend":2.5,"max_budget":10.0,"budget_reset_at":null}}"#,
        );
        http.answer("/health/liveliness", 200, "\"I'm alive!\"");
        let proxy = RelayProxy::new(config).with_account(Arc::clone(&account));

        let before = proxy
            .status_payload(json!({"state": "ok"}))
            .expect("status payload should build");
        assert_eq!(before["account"]["teams"], Value::Null);
        assert_eq!(before["account"]["gateway"]["reachable"], json!(false));
        assert_eq!(
            before["environments"],
            json!({"current": "dev", "available": [{"name": "dev", "url": settings.gateway.url}]})
        );

        account.poll();
        let after = proxy
            .status_payload(json!({"state": "ok"}))
            .expect("status payload should build");
        assert_eq!(after["account"]["user"]["email"], json!("dev@example.com"));
        assert_eq!(
            after["account"]["teams"],
            json!([{"id": "team-a", "alias": "Team A"}])
        );
        assert_eq!(after["account"]["team"]["spend"], json!(2.5));
        assert_eq!(after["account"]["team"]["max_budget"], json!(10.0));
        assert_eq!(after["account"]["gateway"]["reachable"], json!(true));
        assert!(!after.to_string().contains("sk-status-secret"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn should_report_the_mcp_catalog_size_and_servers_on_status() {
        use crate::mcp::{
            catalog::tests::tool,
            test_support::{service_on, FakeUpstream},
        };

        let settings = static_key_settings("sk-status-secret");
        let mut config = settings.to_config();
        config.mitm_enabled = false;
        config.log_path = std::env::temp_dir().join("relay-status-test-missing.log.jsonl");
        let upstream = FakeUpstream::serving(vec![
            tool("github-get_issue", "Read one issue"),
            tool("jira-search", "Search issues"),
        ]);
        let (_rig, service) = service_on(settings, &upstream);
        let proxy = RelayProxy::new(config).with_mcp(Arc::clone(&service));
        let before = proxy
            .status_payload(json!({"state": "ok"}))
            .expect("payload");
        assert_eq!(before["mcp"], json!({"catalog_tools": null, "servers": {}}));

        service.check_session().await;
        let after = proxy
            .status_payload(json!({"state": "ok"}))
            .expect("payload");
        assert_eq!(
            after["mcp"],
            json!({
                "catalog_tools": 2,
                "servers": {
                    "github": {"tools": 1, "active": false},
                    "jira": {"tools": 1, "active": false},
                },
            })
        );
        assert!(!after.to_string().contains("sk-status-secret"));
    }

    #[test]
    fn should_report_the_broker_state_on_status_without_any_token() {
        let settings = static_key_settings("sk-status-secret");
        let mut config = settings.to_config();
        config.mitm_enabled = false;
        config.log_path = std::env::temp_dir().join("relay-status-test-missing.log.jsonl");
        let rig = Rig::new(settings);
        let proxy = RelayProxy::new(config).with_broker(Arc::clone(&rig.broker));

        let before = proxy
            .status_payload(json!({"state": "ok"}))
            .expect("status payload should build");
        assert_eq!(before["broker"]["signed_in"], json!(true));
        assert_eq!(before["broker"]["source"], Value::Null);

        rig.broker.handle(
            Request::Credential {
                context: Context::Interactive,
            },
            rig.peer(),
        );
        let after = proxy
            .status_payload(json!({"state": "ok"}))
            .expect("status payload should build");

        assert_eq!(after["broker"]["source"], json!("static_key"));
        assert_eq!(after["broker"]["refused_callers"], json!(0));
        assert!(!after.to_string().contains("sk-status-secret"));
    }

    use std::{sync::Mutex, time::Duration};

    use tokio::{io::AsyncReadExt, sync::Notify, time::timeout};

    const GATEWAY_KEY: &str = "sk-gateway-secret";
    const PATIENCE: Duration = Duration::from_secs(10);

    struct Upstream {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
        release: Arc<Notify>,
    }

    impl Upstream {
        async fn answering(parts: &[&str]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let release = Arc::new(Notify::new());
            let parts = parts
                .iter()
                .map(|part| part.to_string())
                .collect::<Vec<_>>();
            let (seen, gate) = (Arc::clone(&requests), Arc::clone(&release));
            tokio::spawn(async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let message = read_http_message(&mut stream).await.unwrap().unwrap();
                    seen.lock().unwrap().push(format!(
                        "{}{}",
                        message.header_text,
                        String::from_utf8_lossy(&message.body)
                    ));
                    for (index, part) in parts.iter().enumerate() {
                        if index > 0 {
                            gate.notified().await;
                        }
                        stream.write_all(part.as_bytes()).await.unwrap();
                    }
                    stream.shutdown().await.unwrap();
                }
            });
            Self {
                url,
                requests,
                release,
            }
        }

        fn requests(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    struct Daemon {
        proxy: Arc<RelayProxy>,
        rig: Rig,
        token: String,
    }

    fn daemon(gateway_url: &str) -> Daemon {
        let mut settings = static_key_settings(GATEWAY_KEY);
        settings.gateway.url = gateway_url.to_string();
        let mut config = settings.to_config();
        config.mitm_enabled = false;
        config.log_path = std::env::temp_dir().join("relay-inference-test-missing.log.jsonl");
        let rig = Rig::new(settings);
        let token = match rig.broker.handle(
            Request::ProxyCredential {
                context: Context::Interactive,
            },
            rig.peer(),
        ) {
            Reply::Credential(issued) => issued.token,
            other => panic!("expected a proxy token, got {other:?}"),
        };
        Daemon {
            proxy: Arc::new(RelayProxy::new(config).with_broker(Arc::clone(&rig.broker))),
            rig,
            token,
        }
    }

    async fn connect(proxy: &Arc<RelayProxy>, claimed_peer: Option<&str>) -> TcpStream {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (stream, peer) = listener.accept().await.unwrap();
        let peer = claimed_peer.map_or(peer, |address| address.parse().unwrap());
        let proxy = Arc::clone(proxy);
        tokio::spawn(async move { proxy.handle_client(stream, peer).await });
        client
    }

    async fn exchange(
        proxy: &Arc<RelayProxy>,
        claimed_peer: Option<&str>,
        request: &str,
    ) -> String {
        let mut client = connect(proxy, claimed_peer).await;
        client.write_all(request.as_bytes()).await.unwrap();
        let mut answer = Vec::new();
        timeout(PATIENCE, client.read_to_end(&mut answer))
            .await
            .expect("the daemon must answer and close")
            .unwrap();
        String::from_utf8_lossy(&answer).to_string()
    }

    fn header_lines<'a>(message: &'a str, name: &str) -> Vec<&'a str> {
        message
            .split("\r\n\r\n")
            .next()
            .unwrap()
            .split("\r\n")
            .skip(1)
            .filter_map(|line| line.split_once(':'))
            .filter(|(candidate, _)| candidate.trim().eq_ignore_ascii_case(name))
            .map(|(_, value)| value.trim())
            .collect()
    }

    const JSON_OK: &str =
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nx-litellm-call-id: call-1\r\n\r\n{\"ok\":true}";

    #[tokio::test]
    async fn should_forward_v1_with_the_gateway_credential_in_every_header_the_client_used() {
        let upstream = Upstream::answering(&[JSON_OK]).await;
        let daemon = daemon(&upstream.url);
        let token = &daemon.token;
        let request = format!(
            "POST /v1/messages?beta=true&next=%2Fa%20b HTTP/1.1\r\nHost: 127.0.0.1:4142\r\nAuthorization: Bearer {token}\r\nX-Api-Key: {token}\r\nanthropic-beta: one\r\nanthropic-beta: two\r\nx-litellm-team: team-a\r\nUser-Agent: claude-cli/2.1.291\r\nConnection: keep-alive, x-per-hop\r\nx-per-hop: 1\r\nContent-Type: application/json\r\nContent-Length: 17\r\n\r\n{{\"model\":\"haiku\"}}"
        );

        let answer = exchange(&daemon.proxy, None, &request).await;

        let seen = upstream.requests();
        assert_eq!(seen.len(), 1);
        let forwarded = &seen[0];
        assert!(
            forwarded.starts_with("POST /v1/messages?beta=true&next=%2Fa%20b HTTP/1.1\r\n"),
            "{forwarded}"
        );
        assert_eq!(
            header_lines(forwarded, "authorization"),
            [format!("Bearer {GATEWAY_KEY}")]
        );
        assert_eq!(header_lines(forwarded, "x-api-key"), [GATEWAY_KEY]);
        assert!(!forwarded.contains(token.as_str()));
        assert_eq!(header_lines(forwarded, "anthropic-beta"), ["one", "two"]);
        assert_eq!(header_lines(forwarded, "x-litellm-team"), ["team-a"]);
        assert_eq!(
            header_lines(forwarded, "user-agent"),
            ["claude-cli/2.1.291"]
        );
        assert_eq!(
            header_lines(forwarded, "host"),
            [upstream.url.trim_start_matches("http://")]
        );
        assert!(header_lines(forwarded, "x-per-hop").is_empty());
        assert!(forwarded.ends_with("\r\n\r\n{\"model\":\"haiku\"}"));
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        assert_eq!(header_lines(&answer, "content-length"), ["11"]);
        assert_eq!(header_lines(&answer, "x-litellm-call-id"), ["call-1"]);
        assert!(answer.ends_with("\r\n\r\n{\"ok\":true}"));
        assert!(!answer.contains(GATEWAY_KEY));
    }

    #[tokio::test]
    async fn should_inject_only_the_authorization_header_for_a_bearer_only_client() {
        let upstream = Upstream::answering(&[JSON_OK]).await;
        let daemon = daemon(&upstream.url);
        let request = format!(
            "GET /v1/models?client_version=0.156.1 HTTP/1.1\r\nHost: 127.0.0.1:4142\r\nauthorization: bearer {}\r\n\r\n",
            daemon.token
        );

        let answer = exchange(&daemon.proxy, None, &request).await;

        let seen = upstream.requests();
        assert!(seen[0].starts_with("GET /v1/models?client_version=0.156.1 HTTP/1.1\r\n"));
        assert_eq!(
            header_lines(&seen[0], "authorization"),
            [format!("Bearer {GATEWAY_KEY}")]
        );
        assert!(header_lines(&seen[0], "x-api-key").is_empty());
        assert!(answer.ends_with("{\"ok\":true}"));
    }

    #[tokio::test]
    async fn should_refuse_a_request_that_does_not_carry_the_proxy_token_and_send_nothing_upstream()
    {
        let upstream = Upstream::answering(&[JSON_OK]).await;
        let daemon = daemon(&upstream.url);
        let token = &daemon.token;
        let body = "Content-Length: 2\r\n\r\n{}";
        for (credentials, peer, status) in [
            (String::new(), None, "401 Unauthorized"),
            (
                "Authorization: Bearer relay-proxy-guess\r\n".to_string(),
                None,
                "401 Unauthorized",
            ),
            (
                format!("Authorization: Bearer {GATEWAY_KEY}\r\n"),
                None,
                "401 Unauthorized",
            ),
            (
                format!("Authorization: Basic {token}\r\n"),
                None,
                "401 Unauthorized",
            ),
            (
                format!("Authorization: Bearer {token}\r\nx-api-key: {GATEWAY_KEY}\r\n"),
                None,
                "401 Unauthorized",
            ),
            (
                format!("Authorization: Bearer {token}\r\n"),
                Some("203.0.113.9:50000"),
                "403 Forbidden",
            ),
        ] {
            let request = format!("POST /v1/responses HTTP/1.1\r\nHost: x\r\n{credentials}{body}");
            let answer = exchange(&daemon.proxy, peer, &request).await;
            assert!(
                answer.starts_with(&format!("HTTP/1.1 {status}\r\n")),
                "{credentials:?} from {peer:?} answered {answer}"
            );
            let error: Value =
                serde_json::from_str(answer.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert!(error["error"]["message"]
                .as_str()
                .is_some_and(|text| !text.is_empty()));
            assert!(!answer.contains(GATEWAY_KEY));
        }
        assert!(upstream.requests().is_empty());

        daemon.rig.broker.sign_out();
        let request =
            format!("POST /v1/responses HTTP/1.1\r\nAuthorization: Bearer {token}\r\n{body}");
        let answer = exchange(&daemon.proxy, None, &request).await;
        assert!(
            answer.starts_with("HTTP/1.1 401 Unauthorized\r\n"),
            "{answer}"
        );
        assert!(answer.contains("authentication_error"));
        assert!(upstream.requests().is_empty());
    }

    #[tokio::test]
    async fn should_stream_each_gateway_chunk_to_the_client_before_the_next_one_exists() {
        let upstream = Upstream::answering(&[
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n10\r\nevent: one\n\ndata\r\n",
            "c\r\nevent: two\n\n\r\n0\r\n\r\n",
        ])
        .await;
        let daemon = daemon(&upstream.url);
        let mut client = connect(&daemon.proxy, None).await;
        let request = format!(
            "POST /v1/messages HTTP/1.1\r\nx-api-key: {}\r\nContent-Length: 2\r\n\r\n{{}}",
            daemon.token
        );
        client.write_all(request.as_bytes()).await.unwrap();

        let mut received = Vec::new();
        while !String::from_utf8_lossy(&received).contains("event: one\n\ndata") {
            let mut buffer = [0u8; 1024];
            let read = timeout(PATIENCE, client.read(&mut buffer))
                .await
                .expect("the first event must arrive while the Gateway is still holding the second")
                .unwrap();
            assert!(read > 0, "the stream closed before the first event");
            received.extend_from_slice(&buffer[..read]);
        }
        let early = String::from_utf8_lossy(&received).to_string();
        assert!(!early.contains("event: two"));
        assert_eq!(header_lines(&early, "transfer-encoding"), ["chunked"]);
        assert_eq!(header_lines(&early, "content-type"), ["text/event-stream"]);

        upstream.release.notify_one();
        timeout(PATIENCE, client.read_to_end(&mut received))
            .await
            .expect("the stream must end once the Gateway ends it")
            .unwrap();
        let whole = String::from_utf8_lossy(&received).to_string();
        assert!(whole.contains("event: two\n\n"));
        assert!(whole.ends_with("0\r\n\r\n"));
    }

    #[tokio::test]
    async fn should_pass_a_gateway_error_through_and_answer_502_when_the_gateway_is_unreachable() {
        let upstream = Upstream::answering(&[
            "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\nretry-after: 7\r\ncontent-length: 28\r\n\r\n{\"error\":{\"message\":\"slow\"}}",
        ])
        .await;
        let daemon = daemon(&upstream.url);
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: 2\r\n\r\n{{}}",
            daemon.token
        );
        let answer = exchange(&daemon.proxy, None, &request).await;
        assert!(
            answer.starts_with("HTTP/1.1 429 Too Many Requests\r\n"),
            "{answer}"
        );
        assert_eq!(header_lines(&answer, "retry-after"), ["7"]);
        assert!(answer.ends_with("{\"error\":{\"message\":\"slow\"}}"));

        let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unreachable = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let daemon = self::daemon(&unreachable);
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nAuthorization: Bearer {}\r\nContent-Length: 2\r\n\r\n{{}}",
            daemon.token
        );
        let answer = exchange(&daemon.proxy, None, &request).await;
        assert!(
            answer.starts_with("HTTP/1.1 502 Bad Gateway\r\n"),
            "{answer}"
        );
        assert!(answer.contains("api_error"));
        assert!(!answer.contains(GATEWAY_KEY));
    }

    #[tokio::test]
    async fn should_forward_nothing_outside_v1_even_with_a_valid_token() {
        let upstream = Upstream::answering(&[JSON_OK]).await;
        let daemon = daemon(&upstream.url);
        let nested_hundred_thousand_layers = format!("/v1/%{}2e/key/info", "25".repeat(100_000));
        for target in [
            nested_hundred_thousand_layers.as_str(),
            "/v1/../key/info",
            "/v1/%2e%2e/key/info",
            "/v1/%2e%2e%2fkey/info",
            "/v1/..%2Fkey/info",
            "/v1/%252e%252e/key/info",
            "/v1/..\\key/info",
            "/v1/./key/info",
            "/key/info",
            "/v11/models",
            "/mcp",
        ] {
            assert!(!is_inference_target(target), "{target}");
            let request = format!(
                "GET {target} HTTP/1.1\r\nAuthorization: Bearer {}\r\n\r\n",
                daemon.token
            );
            let answer = exchange(&daemon.proxy, None, &request).await;
            assert!(
                answer.starts_with("HTTP/1.1 501 "),
                "{target} answered {answer}"
            );
        }
        assert!(upstream.requests().is_empty());
        let request = format!(
            "GET /v1/models HTTP/1.1\r\nAuthorization: Bearer {}\r\n\r\n",
            daemon.token
        );
        let answer = exchange(&daemon.proxy, None, &request).await;
        assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"), "{answer}");
        assert_eq!(upstream.requests().len(), 1);
        for target in [
            "/v1",
            "/v1?x=1",
            "/v1/messages?beta=true",
            "/v1/responses",
            "/v1/models/claude%2Dsonnet",
        ] {
            assert!(is_inference_target(target), "{target}");
        }
    }
}
