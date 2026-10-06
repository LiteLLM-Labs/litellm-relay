//! The short-lived Gateway key the broker mints for the managed team and keeps
//! extending, plus the HTTP client that talks to the Gateway's key routes.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::ai_tools::blocking::call;

pub const KEY_DURATION: &str = "60m";
pub const KEY_LIFETIME_SECONDS: i64 = 60 * 60;
pub const KEY_HALF_LIFE_SECONDS: i64 = KEY_LIFETIME_SECONDS / 2;
const KEY_SOURCE: &str = "litellm-relay";
const TEAM_PERMISSION_MARKER: &str = "team_member_permission";
const HTTP_TIMEOUT_SECONDS: u64 = 30;
const HTTP_CONNECT_TIMEOUT_SECONDS: u64 = 10;
const REFUSAL_EXCERPT_CHARS: usize = 240;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintRequest {
    pub team_id: String,
    pub alias: String,
    pub hostname: String,
}

impl MintRequest {
    pub fn new(team_id: &str, hostname: &str, now: i64) -> Self {
        let stamp = DateTime::<Utc>::from_timestamp(now, 0)
            .map(|at| at.format("%Y%m%dT%H%M%SZ").to_string())
            .unwrap_or_else(|| now.to_string());
        Self {
            team_id: team_id.to_string(),
            alias: format!("relay-{hostname}-{stamp}"),
            hostname: hostname.to_string(),
        }
    }

    pub fn body(&self) -> Value {
        json!({
            "duration": KEY_DURATION,
            "team_id": self.team_id,
            "key_alias": self.alias,
            "metadata": {
                "source": KEY_SOURCE,
                "hostname": self.hostname,
                "relay_version": env!("CARGO_PKG_VERSION"),
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minted {
    pub token: String,
    pub expires_at: Option<i64>,
}

/// Why the Gateway would not mint. `Unauthorized` is a plain 401: the bearer
/// itself is dead (the Gateway's sealing key rotated), so the session
/// credential has to be exchanged again before anything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintRefusal {
    TeamPermission(String),
    Unauthorized(String),
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintOutcome {
    Minted(Minted),
    Refused(MintRefusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtendOutcome {
    Extended { expires_at: Option<i64> },
    Unauthorized(String),
}

pub trait KeyServer {
    fn mint(&self, gateway_url: &str, bearer: &str, request: &MintRequest) -> Result<MintOutcome>;
    fn extend(&self, gateway_url: &str, bearer: &str, key: &str) -> Result<ExtendOutcome>;
    fn delete(&self, gateway_url: &str, bearer: &str, key: &str) -> Result<()>;
}

pub struct HttpKeyServer;

struct Reply {
    status: u16,
    body: String,
}

#[derive(Deserialize)]
struct KeyReply {
    key: String,
    #[serde(default)]
    expires: Option<String>,
}

#[derive(Deserialize)]
struct UpdateReply {
    #[serde(default)]
    expires: Option<String>,
}

impl KeyServer for HttpKeyServer {
    fn mint(&self, gateway_url: &str, bearer: &str, request: &MintRequest) -> Result<MintOutcome> {
        let reply = post(gateway_url, "/key/generate", bearer, request.body())?;
        parse_mint(reply)
    }

    fn extend(&self, gateway_url: &str, bearer: &str, key: &str) -> Result<ExtendOutcome> {
        let body = json!({ "key": key, "duration": KEY_DURATION });
        let reply = post(gateway_url, "/key/update", bearer, body)?;
        parse_extension(reply)
    }

    fn delete(&self, gateway_url: &str, bearer: &str, key: &str) -> Result<()> {
        let body = json!({ "keys": [key] });
        let reply = post(gateway_url, "/key/delete", bearer, body)?;
        if !(200..300).contains(&reply.status) {
            anyhow::bail!(
                "key deletion refused: {}",
                excerpt(reply.status, &reply.body)
            );
        }
        Ok(())
    }
}

fn parse_mint(reply: Reply) -> Result<MintOutcome> {
    if (200..300).contains(&reply.status) {
        let key: KeyReply =
            serde_json::from_str(&reply.body).context("the key generate answer was not JSON")?;
        return Ok(MintOutcome::Minted(Minted {
            token: key.key,
            expires_at: key.expires.as_deref().and_then(parse_expiry),
        }));
    }
    let message = excerpt(reply.status, &reply.body);
    let refusal = match reply.status {
        401 | 403 if reply.body.contains(TEAM_PERMISSION_MARKER) => {
            MintRefusal::TeamPermission(message)
        }
        401 => MintRefusal::Unauthorized(message),
        _ => MintRefusal::Other(message),
    };
    Ok(MintOutcome::Refused(refusal))
}

fn parse_extension(reply: Reply) -> Result<ExtendOutcome> {
    match reply.status {
        200..=299 => {
            let update: UpdateReply =
                serde_json::from_str(&reply.body).context("the key update answer was not JSON")?;
            Ok(ExtendOutcome::Extended {
                expires_at: update.expires.as_deref().and_then(parse_expiry),
            })
        }
        401 => Ok(ExtendOutcome::Unauthorized(excerpt(
            reply.status,
            &reply.body,
        ))),
        status => anyhow::bail!("key extension refused: {}", excerpt(status, &reply.body)),
    }
}

pub fn parse_expiry(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|at| at.timestamp())
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|naive| naive.and_utc().timestamp())
        })
}

fn excerpt(status: u16, body: &str) -> String {
    let trimmed: String = body.chars().take(REFUSAL_EXCERPT_CHARS).collect();
    format!("HTTP {status}: {}", trimmed.trim())
}

fn post(gateway_url: &str, route: &str, bearer: &str, body: Value) -> Result<Reply> {
    let url = format!("{}{route}", gateway_url.trim_end_matches('/'));
    let bearer = bearer.to_string();
    call(move || async move {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECONDS))
            .connect_timeout(Duration::from_secs(HTTP_CONNECT_TIMEOUT_SECONDS))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build the gateway HTTP client")?;
        let response = client
            .post(&url)
            .bearer_auth(bearer)
            .json(&body)
            .send()
            .await
            .context("the gateway key request failed")?;
        let status = response.status().as_u16();
        let body = response
            .text()
            .await
            .context("failed to read the gateway key answer")?;
        Ok(Reply { status, body })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_build_the_mint_body_from_the_team_host_and_time() {
        let request = MintRequest::new("team-a", "laptop", 1_800_000_000);
        let body = request.body();
        assert_eq!(body["duration"], "60m");
        assert_eq!(body["team_id"], "team-a");
        assert_eq!(body["key_alias"], "relay-laptop-20270115T080000Z");
        assert_eq!(body["metadata"]["source"], "litellm-relay");
        assert_eq!(body["metadata"]["hostname"], "laptop");
        assert_eq!(body["metadata"]["relay_version"], env!("CARGO_PKG_VERSION"));
        assert!(body.get("models").is_none());
    }

    #[test]
    fn should_classify_a_team_permission_refusal_apart_from_other_failures() {
        let refused = parse_mint(Reply {
            status: 401,
            body: r#"{"error":{"message":"team_member_permission: /key/generate not allowed"}}"#
                .into(),
        })
        .expect("classified");
        assert!(matches!(
            refused,
            MintOutcome::Refused(MintRefusal::TeamPermission(_))
        ));
        let other = parse_mint(Reply {
            status: 400,
            body: "bad alias".into(),
        })
        .expect("classified");
        assert!(
            matches!(other, MintOutcome::Refused(MintRefusal::Other(message)) if message == "HTTP 400: bad alias")
        );
        let unauthorized = parse_mint(Reply {
            status: 401,
            body: "LiteLLM Virtual Key expected".into(),
        })
        .expect("classified");
        assert!(matches!(
            unauthorized,
            MintOutcome::Refused(MintRefusal::Unauthorized(_))
        ));
    }

    #[test]
    fn should_tell_a_dead_bearer_apart_from_a_failed_extension() {
        let extended = parse_extension(Reply {
            status: 200,
            body: r#"{"key":"sk-test","expires":"2027-01-15T09:00:00+00:00"}"#.into(),
        })
        .expect("parsed");
        assert_eq!(
            extended,
            ExtendOutcome::Extended {
                expires_at: Some(1_800_003_600)
            }
        );
        let dead = parse_extension(Reply {
            status: 401,
            body: "LiteLLM Virtual Key expected".into(),
        })
        .expect("classified");
        assert!(matches!(dead, ExtendOutcome::Unauthorized(_)));
        assert!(parse_extension(Reply {
            status: 500,
            body: "boom".into(),
        })
        .is_err());
    }

    #[test]
    fn should_read_the_key_and_its_expiry_from_a_mint_answer() {
        let minted = parse_mint(Reply {
            status: 200,
            body: r#"{"key":"sk-test","expires":"2027-01-15T09:00:00.000000+00:00"}"#.into(),
        })
        .expect("parsed");
        assert_eq!(
            minted,
            MintOutcome::Minted(Minted {
                token: "sk-test".into(),
                expires_at: Some(1_800_003_600),
            })
        );
        assert_eq!(
            parse_expiry("2027-01-15T09:00:00.000000"),
            Some(1_800_003_600)
        );
        assert_eq!(parse_expiry("never"), None);
    }
}
