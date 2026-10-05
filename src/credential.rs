//! Liveness and expiry of the Gateway session credential that `relay setup` saves.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use reqwest::StatusCode;
use serde_json::Value;

pub const EXPIRY_WARNING_WINDOW: Duration = Duration::hours(24);
pub const REENROLL_HINT: &str = "Run `litellm-relay setup` to sign in again.";
pub const CREDENTIAL_CHECK_TIMEOUT: StdDuration = StdDuration::from_secs(10);

#[derive(Clone, Debug, PartialEq)]
pub enum CredentialCheck {
    Valid,
    /// Authenticated, but the Gateway does not allow this key on the probe route.
    Restricted {
        detail: String,
    },
    Rejected {
        detail: String,
    },
    Unverifiable {
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExpiryState {
    Unknown,
    Ok { at: DateTime<Utc> },
    ExpiringSoon { at: DateTime<Utc> },
    Expired { at: DateTime<Utc> },
}

/// HTTP client for credential probes, bounded so a silent Gateway never hangs a caller.
pub fn check_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(CREDENTIAL_CHECK_TIMEOUT)
        .build()
        .expect("reqwest client configuration should be valid")
}

/// Ask the Gateway whether it still accepts `api_key`: 2xx is valid, 401 is a
/// rejection, 403 is an authenticated key the Gateway will not serve on this
/// route, and everything else leaves the question open.
pub async fn check_credential(
    http: &reqwest::Client,
    gateway_url: &str,
    api_key: &str,
) -> CredentialCheck {
    let url = format!("{}/v1/models", gateway_url.trim_end_matches('/'));
    let response = match http.get(url).bearer_auth(api_key).send().await {
        Ok(response) => response,
        Err(error) => {
            return CredentialCheck::Unverifiable {
                detail: error.to_string(),
            }
        }
    };
    let status = response.status();
    if status.is_success() {
        return CredentialCheck::Valid;
    }
    let message = gateway_message(&response.text().await.unwrap_or_default());
    match status {
        StatusCode::UNAUTHORIZED => CredentialCheck::Rejected {
            detail: message.unwrap_or_else(|| status.to_string()),
        },
        StatusCode::FORBIDDEN => CredentialCheck::Restricted {
            detail: message.unwrap_or_else(|| status.to_string()),
        },
        _ => CredentialCheck::Unverifiable {
            detail: match message {
                Some(message) => format!("HTTP {}: {message}", status.as_u16()),
                None => format!("HTTP {}", status.as_u16()),
            },
        },
    }
}

/// The Gateway's own explanation, from either its `{"error":{"message"}}` or its
/// FastAPI `{"detail"}` error shape.
fn gateway_message(body: &str) -> Option<String> {
    let json = serde_json::from_str::<Value>(body).ok()?;
    json.get("error")
        .and_then(|error| error.get("message"))
        .or_else(|| json.get("detail"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub fn expiry_state(expires_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> ExpiryState {
    let Some(at) = expires_at else {
        return ExpiryState::Unknown;
    };
    if at <= now {
        return ExpiryState::Expired { at };
    }
    if at - now <= EXPIRY_WARNING_WINDOW {
        return ExpiryState::ExpiringSoon { at };
    }
    ExpiryState::Ok { at }
}

#[cfg(test)]
pub(crate) mod test_support {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    /// Serve one HTTP response and return the base URL to reach it.
    pub(crate) async fn serve_once(status_line: &str, body: &str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 4096];
            let _ = stream.read(&mut request).await;
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
        });
        base_url
    }

    /// Serve each (status line, body) pair to one connection in order and
    /// return the base URL to reach it.
    pub(crate) async fn serve_sequence(responses: &[(&str, &str)]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let responses: Vec<String> = responses
            .iter()
            .map(|(status_line, body)| {
                format!(
                    "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            })
            .collect();
        tokio::spawn(async move {
            for response in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = vec![0u8; 4096];
                let _ = stream.read(&mut request).await;
                stream.write_all(response.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
        });
        base_url
    }

    /// Answer the first connection, then hold the second open without answering
    /// until `release` fires.
    pub(crate) async fn serve_once_then_hung(
        status_line: &str,
        body: &str,
        release: tokio::sync::oneshot::Receiver<()>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 4096];
            let _ = stream.read(&mut request).await;
            stream.write_all(response.as_bytes()).await.unwrap();
            stream.shutdown().await.unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            let _ = release.await;
            drop(stream);
        });
        base_url
    }

    pub(crate) async fn unused_port_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        base_url
    }
}

#[cfg(test)]
mod tests {
    use super::{
        test_support::{serve_once, unused_port_url},
        *,
    };

    const EXPIRED_KEY_BODY: &str = r#"{"error":{"message":"Authentication Error - Expired Key. Key Expiry time 2026-09-21 22:27:58.096000+00:00 and current time 2026-09-21 22:28:15.211000+00:00","type":"expired_key","code":"401"}}"#;
    const ROUTE_NOT_ALLOWED_BODY: &str = r#"{"detail":"Virtual key is not allowed to call this route. Only allowed to call routes: ['/chat/completions', '/v1/chat/completions']. Tried to call route: /v1/models"}"#;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[tokio::test]
    async fn should_accept_credential_the_gateway_accepts() {
        let gateway = serve_once("200 OK", r#"{"data":[]}"#).await;
        assert_eq!(
            check_credential(&check_client(), &gateway, "sk-live").await,
            CredentialCheck::Valid
        );
    }

    #[tokio::test]
    async fn should_reject_credential_with_the_gateway_message() {
        let gateway = serve_once("401 Unauthorized", EXPIRED_KEY_BODY).await;
        let check = check_credential(&check_client(), &format!("{gateway}/"), "sk-expired").await;
        let CredentialCheck::Rejected { detail } = check else {
            panic!("expected Rejected, got {check:?}");
        };
        assert!(
            detail.starts_with("Authentication Error - Expired Key"),
            "{detail}"
        );
        assert!(!detail.contains("sk-expired"));
    }

    #[tokio::test]
    async fn should_fall_back_to_status_line_when_rejection_body_is_not_json() {
        let gateway = serve_once("401 Unauthorized", "nope").await;
        assert_eq!(
            check_credential(&check_client(), &gateway, "sk-forbidden").await,
            CredentialCheck::Rejected {
                detail: "401 Unauthorized".into(),
            }
        );
    }

    #[tokio::test]
    async fn should_report_route_restricted_key_as_restricted_not_rejected() {
        let gateway = serve_once("403 Forbidden", ROUTE_NOT_ALLOWED_BODY).await;
        assert_eq!(
            check_credential(&check_client(), &gateway, "sk-chat-only").await,
            CredentialCheck::Restricted {
                detail: "Virtual key is not allowed to call this route. Only allowed to call routes: ['/chat/completions', '/v1/chat/completions']. Tried to call route: /v1/models".into(),
            }
        );
    }

    #[tokio::test]
    async fn should_treat_gateway_errors_as_unverifiable() {
        let gateway = serve_once("502 Bad Gateway", "").await;
        assert_eq!(
            check_credential(&check_client(), &gateway, "sk-live").await,
            CredentialCheck::Unverifiable {
                detail: "HTTP 502".into(),
            }
        );
    }

    #[tokio::test]
    async fn should_carry_the_gateway_message_on_unverifiable_statuses() {
        let gateway = serve_once(
            "400 Bad Request",
            r#"{"error":{"message":"Budget has been exceeded! Current cost: 1.2, Max budget: 1.0","type":"budget_exceeded","code":"400"}}"#,
        )
        .await;
        assert_eq!(
            check_credential(&check_client(), &gateway, "sk-live").await,
            CredentialCheck::Unverifiable {
                detail: "HTTP 400: Budget has been exceeded! Current cost: 1.2, Max budget: 1.0"
                    .into(),
            }
        );
    }

    #[tokio::test]
    async fn should_treat_unreachable_gateway_as_unverifiable() {
        let gateway = unused_port_url().await;
        let check = check_credential(&check_client(), &gateway, "sk-live").await;
        let CredentialCheck::Unverifiable { detail } = check else {
            panic!("expected Unverifiable, got {check:?}");
        };
        assert!(!detail.is_empty());
        assert!(!detail.contains("sk-live"));
    }

    #[test]
    fn should_report_unknown_expiry_without_a_timestamp() {
        assert_eq!(
            expiry_state(None, at("2026-09-21T22:00:00Z")),
            ExpiryState::Unknown
        );
    }

    #[test]
    fn should_report_ok_expiry_outside_the_warning_window() {
        let expires_at = at("2026-09-23T22:00:01Z");
        assert_eq!(
            expiry_state(Some(expires_at), at("2026-09-22T22:00:00Z")),
            ExpiryState::Ok { at: expires_at }
        );
    }

    #[test]
    fn should_warn_inside_the_warning_window() {
        let expires_at = at("2026-09-23T22:00:00Z");
        assert_eq!(
            expiry_state(Some(expires_at), at("2026-09-22T22:00:00Z")),
            ExpiryState::ExpiringSoon { at: expires_at }
        );
        assert_eq!(
            expiry_state(Some(expires_at), at("2026-09-23T21:59:59Z")),
            ExpiryState::ExpiringSoon { at: expires_at }
        );
    }

    #[test]
    fn should_report_expired_at_and_after_the_timestamp() {
        let expires_at = at("2026-09-21T22:27:58Z");
        assert_eq!(
            expiry_state(Some(expires_at), expires_at),
            ExpiryState::Expired { at: expires_at }
        );
        assert_eq!(
            expiry_state(Some(expires_at), at("2026-09-21T22:28:15Z")),
            ExpiryState::Expired { at: expires_at }
        );
    }
}
