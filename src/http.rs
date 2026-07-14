use std::{
    collections::HashMap,
    io::{Cursor, Read},
};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::config::normalize_host;

#[derive(Debug)]
pub struct Route {
    pub path: String,
    pub query: String,
}

#[derive(Debug)]
pub struct ConnectTarget {
    pub host: String,
    pub port: u16,
}

#[derive(Debug)]
pub struct HttpMessage {
    pub raw: Vec<u8>,
    pub header_text: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

#[derive(Debug)]
pub struct HttpHead {
    pub raw_headers: Vec<u8>,
    pub header_text: String,
    pub headers: HashMap<String, String>,
}

#[derive(Debug)]
pub struct HttpBody {
    pub raw: Vec<u8>,
    pub decoded: Vec<u8>,
}

#[derive(Debug)]
pub struct RequestLine {
    pub method: String,
    pub path: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolCompatibilityMode {
    BufferedCapture,
    MetadataOnlyTunnel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolCompatibilityReason {
    Http1Buffered,
    ServerSentEvents,
    WebSocketUpgrade,
    HttpUpgrade,
}

impl ProtocolCompatibilityReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http1Buffered => "http1_buffered",
            Self::ServerSentEvents => "server_sent_events",
            Self::WebSocketUpgrade => "websocket_upgrade",
            Self::HttpUpgrade => "http_upgrade",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ProtocolCompatibilityDecision {
    pub mode: ProtocolCompatibilityMode,
    pub reason: ProtocolCompatibilityReason,
}

impl ProtocolCompatibilityDecision {
    pub fn buffered() -> Self {
        Self {
            mode: ProtocolCompatibilityMode::BufferedCapture,
            reason: ProtocolCompatibilityReason::Http1Buffered,
        }
    }

    pub fn metadata_only(reason: ProtocolCompatibilityReason) -> Self {
        Self {
            mode: ProtocolCompatibilityMode::MetadataOnlyTunnel,
            reason,
        }
    }

    pub fn is_metadata_only(self) -> bool {
        self.mode == ProtocolCompatibilityMode::MetadataOnlyTunnel
    }
}

pub async fn write_response<T>(
    stream: &mut T,
    status: u16,
    content_type: &str,
    body: &[u8],
    include_body: bool,
) -> Result<()>
where
    T: AsyncWriteExt + Unpin,
{
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

pub async fn read_until_headers<T>(stream: &mut T) -> Result<Vec<u8>>
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

pub async fn read_http_message<T>(stream: &mut T) -> Result<Option<HttpMessage>>
where
    T: AsyncRead + Unpin,
{
    let head = match read_http_head(stream).await? {
        Some(head) => head,
        None => return Ok(None),
    };
    let body = read_http_body(stream, &head.headers).await?;
    let mut raw = head.raw_headers;
    raw.extend_from_slice(&body.raw);
    Ok(Some(HttpMessage {
        raw,
        header_text: head.header_text,
        headers: head.headers,
        body: body.decoded,
    }))
}

pub async fn read_http_head<T>(stream: &mut T) -> Result<Option<HttpHead>>
where
    T: AsyncRead + Unpin,
{
    let raw_headers = match read_until_headers(stream).await {
        Ok(header) => header,
        Err(_) => return Ok(None),
    };
    let header_text = String::from_utf8_lossy(&raw_headers).to_string();
    let headers = parse_headers(&header_text);
    Ok(Some(HttpHead {
        raw_headers,
        header_text,
        headers,
    }))
}

pub async fn read_http_body<T>(
    stream: &mut T,
    headers: &HashMap<String, String>,
) -> Result<HttpBody>
where
    T: AsyncRead + Unpin,
{
    if has_chunked_transfer_encoding(headers) {
        let (raw, decoded) = read_chunked_body(stream).await?;
        return Ok(HttpBody { raw, decoded });
    }
    if let Some(content_length) = headers.get("content-length") {
        let content_length = content_length
            .parse::<usize>()
            .context("invalid content-length")?;
        let mut body = vec![0; content_length];
        stream.read_exact(&mut body).await?;
        return Ok(HttpBody {
            raw: body.clone(),
            decoded: body,
        });
    }
    Ok(HttpBody {
        raw: Vec::new(),
        decoded: Vec::new(),
    })
}

pub async fn copy_bidirectional_counted<L, R>(left: &mut L, right: &mut R) -> Result<(u64, u64)>
where
    L: AsyncRead + AsyncWrite + Unpin,
    R: AsyncRead + AsyncWrite + Unpin,
{
    Ok(tokio::io::copy_bidirectional(left, right).await?)
}

pub fn parse_start_line(header_text: &str) -> Result<(String, String)> {
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

pub fn parse_route(target: &str) -> Route {
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

pub fn parse_limit(query: &str) -> usize {
    query
        .split('&')
        .find_map(|part| part.strip_prefix("limit="))
        .and_then(|raw| raw.parse::<usize>().ok())
        .map(|value| value.clamp(1, 1000))
        .unwrap_or(250)
}

pub fn parse_connect_target(target: &str) -> Result<ConnectTarget> {
    if let Some(rest) = target.strip_prefix('[') {
        let (host, rest) = rest
            .split_once(']')
            .ok_or_else(|| anyhow!("invalid IPv6 target"))?;
        let port = rest.trim_start_matches(':').parse::<u16>().unwrap_or(443);
        return Ok(ConnectTarget {
            host: host.to_string(),
            port,
        });
    }
    let (host, port) = target
        .split_once(':')
        .map(|(host, port)| (host, port.parse::<u16>().unwrap_or(443)))
        .unwrap_or((target, 443));
    Ok(ConnectTarget {
        host: normalize_host(host),
        port,
    })
}

pub fn parse_request_line(header_text: &str) -> RequestLine {
    let first = header_text.lines().next().unwrap_or_default();
    let mut parts = first.splitn(3, ' ');
    RequestLine {
        method: parts.next().unwrap_or("UNKNOWN").to_string(),
        path: parts.next().unwrap_or("").to_string(),
    }
}

pub fn parse_response_status(header_text: &str) -> Option<u16> {
    header_text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
}

pub fn should_close(headers: &HashMap<String, String>) -> bool {
    headers
        .get("connection")
        .is_some_and(|value| value.eq_ignore_ascii_case("close"))
}

pub fn request_protocol_decision(
    headers: &HashMap<String, String>,
) -> ProtocolCompatibilityDecision {
    upgrade_protocol_decision(headers).unwrap_or_else(ProtocolCompatibilityDecision::buffered)
}

pub fn response_protocol_decision(
    status_code: Option<u16>,
    headers: &HashMap<String, String>,
) -> ProtocolCompatibilityDecision {
    if status_code == Some(101) {
        return upgrade_protocol_decision(headers).unwrap_or_else(|| {
            ProtocolCompatibilityDecision::metadata_only(ProtocolCompatibilityReason::HttpUpgrade)
        });
    }
    if let Some(decision) = upgrade_protocol_decision(headers) {
        return decision;
    }
    if is_server_sent_events(headers) {
        return ProtocolCompatibilityDecision::metadata_only(
            ProtocolCompatibilityReason::ServerSentEvents,
        );
    }
    ProtocolCompatibilityDecision::buffered()
}

pub fn build_metadata_only_http_payload(
    headers: &HashMap<String, String>,
    extra: Value,
    decision: ProtocolCompatibilityDecision,
) -> Value {
    let mut payload = match extra {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    payload.insert("body_bytes".into(), json!(0));
    payload.insert("body".into(), Value::Null);
    payload.insert(
        "body_preview".into(),
        json!(format!(
            "[{} response body tunneled without capture]",
            decision.reason.as_str()
        )),
    );
    payload.insert("decoded_body_bytes".into(), json!(0));
    payload.insert("body_truncated".into(), json!(false));
    payload.insert("preview_truncated".into(), json!(false));
    payload.insert("truncated".into(), json!(false));
    payload.insert(
        "body_unavailable_reason".into(),
        json!(format!(
            "{} body tunneled without capture",
            decision.reason.as_str()
        )),
    );
    payload.insert("capture_mode".into(), json!("metadata_only_tunnel"));
    payload.insert(
        "protocol_compatibility_reason".into(),
        json!(decision.reason.as_str()),
    );
    payload.insert("headers".into(), redact_headers(headers));
    Value::Object(payload)
}

pub fn redact_headers(headers: &HashMap<String, String>) -> Value {
    let mut redacted = Map::new();
    for (key, value) in headers {
        let value = if is_sensitive_name(key) {
            "<redacted>"
        } else {
            value
        };
        redacted.insert(key.clone(), Value::String(value.to_string()));
    }
    Value::Object(redacted)
}

pub fn scrub_request_target(target: &str) -> String {
    let parsed = if target.starts_with("http://") || target.starts_with("https://") {
        url::Url::parse(target)
    } else {
        url::Url::parse(&format!("http://relay.local{target}"))
    };
    let Ok(mut url) = parsed else {
        return target.to_string();
    };
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| {
            let value = if is_sensitive_name(key.as_ref()) {
                "<redacted>".to_string()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect();
    url.set_query(None);
    if !pairs.is_empty() {
        let mut query = url.query_pairs_mut();
        for (key, value) in pairs {
            query.append_pair(&key, &value);
        }
    }

    let mut scrubbed = url.path().to_string();
    if let Some(query) = url.query() {
        scrubbed.push('?');
        scrubbed.push_str(query);
    }
    scrubbed
}

pub fn is_sensitive_name(name: &str) -> bool {
    const SENSITIVE_EXACT: [&str; 10] = [
        "authorization",
        "proxy-authorization",
        "cookie",
        "set-cookie",
        "password",
        "passwd",
        "secret",
        "signature",
        "sig",
        "code",
    ];
    const SENSITIVE_PARTS: [&str; 14] = [
        "api-key",
        "apikey",
        "auth",
        "bearer",
        "client-secret",
        "credential",
        "csrf",
        "id-token",
        "jwt",
        "refresh-token",
        "secret",
        "session",
        "token",
        "xsrf",
    ];
    let normalized = name.trim().to_ascii_lowercase().replace('_', "-");
    SENSITIVE_EXACT.contains(&normalized.as_str())
        || SENSITIVE_PARTS.iter().any(|part| normalized.contains(part))
}

pub fn build_http_payload(
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
            let body_value = if body_limit == 0 {
                Value::Null
            } else {
                json!(preview_bytes(&decoded, body_limit))
            };
            payload.insert("body".into(), body_value);
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

fn upgrade_protocol_decision(
    headers: &HashMap<String, String>,
) -> Option<ProtocolCompatibilityDecision> {
    if !header_contains_token(headers, "connection", "upgrade") {
        return None;
    }
    if header_contains_token(headers, "upgrade", "websocket") {
        Some(ProtocolCompatibilityDecision::metadata_only(
            ProtocolCompatibilityReason::WebSocketUpgrade,
        ))
    } else {
        Some(ProtocolCompatibilityDecision::metadata_only(
            ProtocolCompatibilityReason::HttpUpgrade,
        ))
    }
}

fn is_server_sent_events(headers: &HashMap<String, String>) -> bool {
    header_contains_token(headers, "content-type", "text/event-stream")
        || (has_chunked_transfer_encoding(headers)
            && headers.get("content-type").is_some_and(|content_type| {
                let content_type = content_type.to_ascii_lowercase();
                content_type.contains("text/event-stream")
                    || content_type.contains("application/x-ndjson")
                    || content_type.contains("application/json-seq")
                    || content_type.contains("application/stream+json")
            }))
}

fn has_chunked_transfer_encoding(headers: &HashMap<String, String>) -> bool {
    header_contains_token(headers, "transfer-encoding", "chunked")
}

fn header_contains_token(headers: &HashMap<String, String>, key: &str, token: &str) -> bool {
    headers.get(key).is_some_and(|value| {
        value
            .split([',', ';'])
            .any(|part| part.trim().eq_ignore_ascii_case(token))
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_connect_target_defaults_to_tls_port() {
        let target = parse_connect_target("www.notion.so").unwrap();
        assert_eq!(target.host, "www.notion.so");
        assert_eq!(target.port, 443);
    }

    #[test]
    fn parse_limit_clamps_invalid_values() {
        assert_eq!(parse_limit("limit=2"), 2);
        assert_eq!(parse_limit("limit=999999"), 1000);
        assert_eq!(parse_limit("limit=nope"), 250);
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

    #[test]
    fn build_http_payload_omits_body_when_body_limit_is_zero() {
        let headers = HashMap::from([("content-type".to_string(), "application/json".to_string())]);
        let payload = build_http_payload(
            br#"{"prompt":"hello notion"}"#,
            &headers,
            10,
            0,
            json!({"method": "POST", "path": "/api/v3/runInferenceTranscript"}),
        );

        assert_eq!(payload["body"], Value::Null);
        assert_eq!(payload["body_preview"], "{\"prompt\":");
        assert_eq!(payload["body_truncated"], true);
        assert_eq!(payload["preview_truncated"], true);
    }

    #[test]
    fn redact_headers_redacts_secret_style_names() {
        let headers = HashMap::from([
            ("authorization".to_string(), "Bearer token".to_string()),
            ("x-session-id".to_string(), "session-secret".to_string()),
            ("x-api-key".to_string(), "sk-secret".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]);

        let redacted = redact_headers(&headers);

        assert_eq!(redacted["authorization"], "<redacted>");
        assert_eq!(redacted["x-session-id"], "<redacted>");
        assert_eq!(redacted["x-api-key"], "<redacted>");
        assert_eq!(redacted["content-type"], "application/json");
    }

    #[test]
    fn scrub_request_target_redacts_query_secrets() {
        let scrubbed = scrub_request_target(
            "/v1/responses?model=gpt-4o&api_key=sk-secret&session_id=abc&limit=10",
        );

        assert_eq!(
            scrubbed,
            "/v1/responses?model=gpt-4o&api_key=%3Credacted%3E&session_id=%3Credacted%3E&limit=10"
        );
        assert!(!scrubbed.contains("sk-secret"));
        assert!(!scrubbed.contains("abc"));
    }

    #[test]
    fn request_protocol_decision_detects_websocket_upgrade() {
        let headers = HashMap::from([
            ("connection".to_string(), "keep-alive, Upgrade".to_string()),
            ("upgrade".to_string(), "websocket".to_string()),
        ]);

        let decision = request_protocol_decision(&headers);

        assert_eq!(decision.mode, ProtocolCompatibilityMode::MetadataOnlyTunnel);
        assert_eq!(
            decision.reason,
            ProtocolCompatibilityReason::WebSocketUpgrade
        );
    }

    #[test]
    fn response_protocol_decision_tunnels_chunked_sse() {
        let headers = HashMap::from([
            ("transfer-encoding".to_string(), "chunked".to_string()),
            (
                "content-type".to_string(),
                "text/event-stream; charset=utf-8".to_string(),
            ),
        ]);

        let decision = response_protocol_decision(Some(200), &headers);

        assert_eq!(decision.mode, ProtocolCompatibilityMode::MetadataOnlyTunnel);
        assert_eq!(
            decision.reason,
            ProtocolCompatibilityReason::ServerSentEvents
        );
    }

    #[test]
    fn response_protocol_decision_allows_buffered_chunked_json() {
        let headers = HashMap::from([
            ("transfer-encoding".to_string(), "chunked".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]);

        let decision = response_protocol_decision(Some(200), &headers);

        assert_eq!(decision, ProtocolCompatibilityDecision::buffered());
    }

    #[test]
    fn metadata_only_payload_does_not_claim_body_capture() {
        let headers = HashMap::from([
            ("transfer-encoding".to_string(), "chunked".to_string()),
            ("content-type".to_string(), "text/event-stream".to_string()),
        ]);
        let decision = response_protocol_decision(Some(200), &headers);

        let payload =
            build_metadata_only_http_payload(&headers, json!({"status_code": 200}), decision);

        assert_eq!(payload["capture_mode"], "metadata_only_tunnel");
        assert_eq!(
            payload["protocol_compatibility_reason"],
            "server_sent_events"
        );
        assert_eq!(payload["body"], Value::Null);
        assert_eq!(payload["body_bytes"], 0);
        assert!(payload["body_unavailable_reason"]
            .as_str()
            .unwrap()
            .contains("tunneled without capture"));
    }
}
