use percent_encoding::percent_decode_str;
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, CONTENT_LENGTH},
    Method, StatusCode,
};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    time::timeout,
};

use crate::{
    broker::{Broker, ProxyBearer, Refusal},
    http::{parse_route, read_http_body},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REFUSAL_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const REFUSAL_DRAIN_BYTES: u64 = 64 * 1024 * 1024;
const BEARER_SCHEME: &str = "bearer ";
const HOP_BY_HOP: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

const DECODE_ROUNDS: usize = 3;

pub fn gateway_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("reqwest client configuration should be valid")
}

pub fn is_inference_target(target: &str) -> bool {
    let raw_path = target.split('?').next().unwrap_or(target);
    is_inference_path(raw_path)
        && is_inference_path(&parse_route(target).path)
        && decoded_within(raw_path.to_string(), DECODE_ROUNDS).is_some_and(|decoded| {
            is_inference_path(&decoded)
                && !decoded.contains('\\')
                && !decoded
                    .split('/')
                    .any(|segment| segment == "." || segment == "..")
        })
}

fn decoded_within(path: String, rounds_left: usize) -> Option<String> {
    let decoded = percent_decode_str(&path).decode_utf8_lossy().into_owned();
    match (decoded == path, rounds_left) {
        (true, _) => Some(decoded),
        (false, 0) => None,
        (false, _) => decoded_within(decoded, rounds_left - 1),
    }
}

fn is_inference_path(path: &str) -> bool {
    path == "/v1" || path.starts_with("/v1/")
}

#[derive(Debug, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub target: String,
    pub headers: Vec<(String, String)>,
}

impl Head {
    pub fn parse(method: &str, target: &str, header_text: &str) -> Self {
        Self {
            method: method.to_string(),
            target: target.to_string(),
            headers: header_text
                .split("\r\n")
                .skip(1)
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
                .collect(),
        }
    }

    fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.headers
            .iter()
            .filter(move |(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn declared_length(&self) -> u64 {
        self.values("content-length")
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    fn framing(&self) -> HashMap<String, String> {
        ["content-length", "transfer-encoding"]
            .into_iter()
            .filter_map(|name| {
                self.values(name)
                    .next()
                    .map(|value| (name.to_string(), value.to_string()))
            })
            .collect()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Presented {
    Nothing,
    Token(String),
    Conflicting,
}

pub fn presented_token(head: &Head) -> Presented {
    let bearers = head.values("authorization").map(|value| {
        value
            .get(..BEARER_SCHEME.len())
            .filter(|scheme| scheme.eq_ignore_ascii_case(BEARER_SCHEME))
            .map(|_| value[BEARER_SCHEME.len()..].trim())
    });
    let keys = head.values("x-api-key").map(Some);
    let candidates = bearers.chain(keys).collect::<Vec<_>>();
    match candidates.split_first() {
        None => Presented::Nothing,
        Some((Some(first), rest)) if rest.iter().all(|other| other == &Some(*first)) => {
            Presented::Token(first.to_string())
        }
        Some(_) => Presented::Conflicting,
    }
}

pub fn upstream_headers(head: &Head, credential: &str) -> Result<HeaderMap> {
    let named_by_connection = head
        .values("connection")
        .flat_map(|value| value.split(','))
        .map(|name| name.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut headers = HeaderMap::new();
    for (name, value) in &head.headers {
        let lower = name.to_ascii_lowercase();
        if HOP_BY_HOP.contains(&lower.as_str())
            || named_by_connection.contains(&lower)
            || lower == "host"
            || lower == "content-length"
        {
            continue;
        }
        let value = match lower.as_str() {
            "authorization" => format!("Bearer {credential}"),
            "x-api-key" => credential.to_string(),
            _ => value.clone(),
        };
        headers.append(
            HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("request header name {name:?} is not valid"))?,
            HeaderValue::from_str(&value)
                .with_context(|| format!("request header {name} has a value that is not valid"))?,
        );
    }
    Ok(headers)
}

fn refusal_status(refusal: &Refusal) -> StatusCode {
    match refusal {
        Refusal::CallerRefused(_) | Refusal::SignedOut(_) | Refusal::SignInFailed(_) => {
            StatusCode::UNAUTHORIZED
        }
        Refusal::GatewayError(_) | Refusal::BadRequest(_) => StatusCode::BAD_GATEWAY,
    }
}

fn error_type(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNAUTHORIZED => "authentication_error",
        StatusCode::FORBIDDEN => "permission_error",
        StatusCode::BAD_REQUEST => "invalid_request_error",
        _ => "api_error",
    }
}

async fn refuse<S>(stream: &mut S, head: &Head, status: StatusCode, message: &str) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    eprintln!(
        "proxy: {} {} answered {}: {message}",
        head.method,
        parse_route(&head.target).path,
        status.as_u16()
    );
    let body = serde_json::to_vec(&json!({
        "type": "error",
        "error": { "type": error_type(status), "message": message },
    }))?;
    let response_head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("Error"),
        body.len()
    );
    stream.write_all(response_head.as_bytes()).await?;
    stream.write_all(&body).await?;
    stream.shutdown().await?;
    let unread = head.declared_length().min(REFUSAL_DRAIN_BYTES);
    let _ = timeout(
        REFUSAL_DRAIN_TIMEOUT,
        tokio::io::copy(&mut (&mut *stream).take(unread), &mut tokio::io::sink()),
    )
    .await;
    Ok(())
}

async fn bearer(broker: Option<&Arc<Broker>>, token: String) -> Result<ProxyBearer, Refusal> {
    let Some(broker) = broker.map(Arc::clone) else {
        return Err(Refusal::GatewayError(
            "the credential broker is not running in this Relay daemon".to_string(),
        ));
    };
    tokio::task::spawn_blocking(move || broker.bearer_for_proxy(&token))
        .await
        .unwrap_or_else(|_| {
            Err(Refusal::GatewayError(
                "the broker could not answer this request".to_string(),
            ))
        })
}

pub async fn forward<S>(
    stream: &mut S,
    peer: SocketAddr,
    head: Head,
    broker: Option<&Arc<Broker>>,
    client: &reqwest::Client,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if !peer.ip().is_loopback() {
        return refuse(
            stream,
            &head,
            StatusCode::FORBIDDEN,
            "the local inference proxy answers only connections from this device",
        )
        .await;
    }
    let token = match presented_token(&head) {
        Presented::Token(token) => token,
        Presented::Nothing => {
            return refuse(
                stream,
                &head,
                StatusCode::UNAUTHORIZED,
                "no proxy token on this request; get one with `relay credential --proxy`",
            )
            .await
        }
        Presented::Conflicting => {
            return refuse(
                stream,
                &head,
                StatusCode::UNAUTHORIZED,
                "authorization and x-api-key must carry the same proxy token as a bearer",
            )
            .await
        }
    };
    let bearer = match bearer(broker, token).await {
        Ok(bearer) => bearer,
        Err(refusal) => {
            return refuse(stream, &head, refusal_status(&refusal), refusal.message()).await
        }
    };
    let (method, headers) = match (
        Method::from_bytes(head.method.as_bytes()),
        upstream_headers(&head, &bearer.token),
    ) {
        (Ok(method), Ok(headers)) => (method, headers),
        (Err(error), _) => {
            return refuse(stream, &head, StatusCode::BAD_REQUEST, &error.to_string()).await
        }
        (_, Err(error)) => {
            return refuse(stream, &head, StatusCode::BAD_REQUEST, &error.to_string()).await
        }
    };
    let body = read_http_body(stream, &head.framing()).await?.decoded;
    let url = format!("{}{}", bearer.gateway_url, head.target);
    let sent = client
        .request(method.clone(), &url)
        .headers(headers)
        .body(body)
        .send()
        .await;
    let response = match sent {
        Ok(response) => response,
        Err(error) => {
            return refuse(
                stream,
                &head,
                StatusCode::BAD_GATEWAY,
                &format!(
                    "the Relay daemon could not reach the Gateway at {}: {}",
                    bearer.gateway_url,
                    error.without_url()
                ),
            )
            .await
        }
    };
    relay_response(stream, &method, response).await
}

fn response_head(response: &reqwest::Response, framing: &Framing) -> Vec<u8> {
    let status = response.status();
    let copied = response
        .headers()
        .iter()
        .filter(|(name, _)| !HOP_BY_HOP.contains(&name.as_str()) && *name != CONTENT_LENGTH)
        .flat_map(|(name, value)| {
            [
                name.as_str().as_bytes(),
                &b": "[..],
                value.as_bytes(),
                &b"\r\n"[..],
            ]
            .concat()
        });
    let length = match framing {
        Framing::Length(length) => format!("content-length: {length}\r\n"),
        Framing::Chunked => "transfer-encoding: chunked\r\n".to_string(),
        Framing::NoBody(Some(length)) => format!("content-length: {length}\r\n"),
        Framing::NoBody(None) => String::new(),
    };
    format!(
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    )
    .into_bytes()
    .into_iter()
    .chain(copied)
    .chain(length.into_bytes())
    .chain(b"connection: close\r\n\r\n".iter().copied())
    .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum Framing {
    Length(String),
    Chunked,
    NoBody(Option<String>),
}

fn framing(method: &Method, response: &reqwest::Response) -> Framing {
    let status = response.status();
    let length = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bodiless = method == Method::HEAD
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
        || status.is_informational();
    match (bodiless, length) {
        (true, length) => Framing::NoBody(length),
        (false, Some(length)) => Framing::Length(length),
        (false, None) => Framing::Chunked,
    }
}

async fn relay_response<S>(
    stream: &mut S,
    method: &Method,
    mut response: reqwest::Response,
) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    let framing = framing(method, &response);
    stream
        .write_all(&response_head(&response, &framing))
        .await?;
    stream.flush().await?;
    if matches!(framing, Framing::NoBody(_)) {
        return Ok(stream.shutdown().await?);
    }
    while let Some(chunk) = response
        .chunk()
        .await
        .context("the Gateway response ended early")?
    {
        if chunk.is_empty() {
            continue;
        }
        if framing == Framing::Chunked {
            stream
                .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
                .await?;
            stream.write_all(&chunk).await?;
            stream.write_all(b"\r\n").await?;
        } else {
            stream.write_all(&chunk).await?;
        }
        stream.flush().await?;
    }
    if framing == Framing::Chunked {
        stream.write_all(b"0\r\n\r\n").await?;
    }
    stream.shutdown().await?;
    Ok(())
}
