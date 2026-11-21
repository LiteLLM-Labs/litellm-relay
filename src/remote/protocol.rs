use std::{sync::LazyLock, time::SystemTime};

use axum::{http::StatusCode, response::IntoResponse, Json};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub type Result<T> = std::result::Result<T, Failure>;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub enum Platform {
    #[default]
    #[serde(rename = "macOS")]
    MacOs,
    Linux,
    Unknown,
}

impl Platform {
    pub fn current() -> Self {
        match std::env::consts::OS {
            "macos" => Self::MacOs,
            "linux" => Self::Linux,
            _ => Self::Unknown,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::MacOs => "macOS",
            Self::Linux => "Linux",
            Self::Unknown => "Unknown",
        }
    }
}

impl From<&str> for Platform {
    fn from(value: &str) -> Self {
        match value {
            "macOS" => Self::MacOs,
            "Linux" => Self::Linux,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Failure {
    pub status: u16,
    pub error: String,
}

impl Failure {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            error: message.into(),
        }
    }
}

impl IntoResponse for Failure {
    fn into_response(self) -> axum::response::Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({ "error": self.error })),
        )
            .into_response()
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub fn token() -> String {
    URL_SAFE_NO_PAD.encode(
        [
            Uuid::new_v4().as_bytes().as_slice(),
            Uuid::new_v4().as_bytes().as_slice(),
        ]
        .concat(),
    )
}

pub fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

pub fn redact(value: &str) -> String {
    static SECRETS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\b(?:sk-[A-Za-z0-9_-]{16,}|relay-proxy-[A-Za-z0-9_-]{16,}|eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+)").expect("credential pattern")
    });
    SECRETS
        .replace_all(value, "[credential redacted]")
        .into_owned()
}

pub fn public_error(value: &str) -> String {
    if value.to_lowercase().contains("no healthy deployments") {
        return "This model is unavailable on your gateway. Choose another model or try again later.".into();
    }
    redact(value).chars().take(600).collect()
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    pub fn session_id(self, id: &str) -> String {
        format!("{}:{id}", self.name())
    }
}

pub fn split_session(id: &str) -> Result<(Agent, Uuid)> {
    let (agent, id) = id
        .split_once(':')
        .ok_or_else(|| Failure::new(404, "Session not found."))?;
    let agent = match agent {
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        _ => return Err(Failure::new(404, "Session not found.")),
    };
    Uuid::parse_str(id)
        .map(|id| (agent, id))
        .map_err(|_| Failure::new(404, "Session not found."))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub agent: Agent,
    pub title: String,
    pub project: String,
    pub cwd: String,
    pub model: Option<String>,
    pub status: String,
    pub can_send: bool,
    pub can_stop: bool,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Message {
    pub id: String,
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub id: String,
    pub title: String,
    pub text: String,
    pub can_accept: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpiredApproval {
    pub id: String,
    pub tool: String,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detail {
    pub session: Session,
    pub messages: Vec<Message>,
    pub older_messages: bool,
    pub approvals: Vec<Approval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expired_approval: Option<ExpiredApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Detail {
    pub fn bounded(self) -> Self {
        let count = self.messages.len();
        let mut messages = self
            .messages
            .into_iter()
            .rev()
            .take(120)
            .map(|message| Message {
                text: message.text.chars().take(32_000).collect(),
                ..message
            })
            .scan(0, |bytes, message| {
                *bytes += message.text.len();
                (*bytes <= 2 * 1024 * 1024).then_some(message)
            })
            .collect::<Vec<_>>();
        messages.reverse();
        Self {
            older_messages: self.older_messages || count > messages.len(),
            messages,
            ..self
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "lowercase",
    rename_all_fields = "camelCase"
)]
pub enum Action {
    List {
        cursor: Option<String>,
    },
    Read {
        session_id: String,
    },
    Create {
        cwd: String,
        agent: Agent,
    },
    Send {
        session_id: String,
        text: String,
        model: Option<String>,
    },
    Approve {
        session_id: String,
        approval_id: Uuid,
        decision: Decision,
    },
    Interrupt {
        session_id: String,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Accept,
    Decline,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Command {
    pub id: Uuid,
    pub action: Action,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Reply {
    pub id: Uuid,
    pub result: Outcome,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Outcome {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Outcome {
    pub fn from_result(result: Result<Value>) -> Self {
        match result {
            Ok(value) => Self {
                ok: true,
                value: Some(value),
                status: None,
                error: None,
            },
            Err(failure) => Self {
                ok: false,
                value: None,
                status: Some(failure.status),
                error: Some(failure.error),
            },
        }
    }

    pub fn into_result(self) -> Result<Value> {
        match (self.ok, self.value, self.status, self.error) {
            (true, Some(value), None, None) => Ok(value),
            (false, None, Some(status @ 400..=599), Some(error)) => {
                Err(Failure::new(status, error))
            }
            _ => Err(Failure::new(
                502,
                "The device returned an invalid response.",
            )),
        }
    }
}
