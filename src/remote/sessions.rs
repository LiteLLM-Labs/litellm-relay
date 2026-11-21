use std::path::Path;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{
    claude::Claude,
    codex::Codex,
    protocol::{split_session, Action, Agent, Failure, Result, Session},
};

pub struct Sessions {
    pub claude: Claude,
    pub codex: Codex,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    codex: Option<String>,
}

pub fn project(cwd: &str) -> String {
    Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(cwd)
        .into()
}

pub fn validate_directory(cwd: &str) -> Result<()> {
    if !Path::new(cwd).is_absolute() || !Path::new(cwd).is_dir() {
        return Err(Failure::new(
            400,
            "Choose an existing project folder on the device.",
        ));
    }
    Ok(())
}

fn value<T: Serialize>(value: T) -> Result<Value> {
    serde_json::to_value(value)
        .map_err(|_| Failure::new(500, "Cannot encode the session response."))
}

impl Sessions {
    pub async fn execute(&self, action: Action) -> Result<Value> {
        match action {
            Action::List { cursor } => {
                let page = cursor
                    .as_ref()
                    .map(|cursor| {
                        URL_SAFE_NO_PAD
                            .decode(cursor)
                            .ok()
                            .and_then(|bytes| serde_json::from_slice::<Cursor>(&bytes).ok())
                            .ok_or_else(|| Failure::new(400, "Refresh the session list."))
                    })
                    .transpose()?;
                let (codex, claude) = tokio::join!(
                    self.codex
                        .list(page.as_ref().and_then(|cursor| cursor.codex.as_deref())),
                    async {
                        if page.is_some() {
                            Ok(Vec::new())
                        } else {
                            self.claude.list().await
                        }
                    }
                );
                if codex.is_err() && claude.is_err() {
                    return Err(Failure::new(
                        503,
                        "Neither coding agent is available on this device.",
                    ));
                }
                let warnings = [codex.as_ref().err(), claude.as_ref().err()]
                    .into_iter()
                    .flatten()
                    .map(|failure| failure.error.clone())
                    .collect::<Vec<_>>();
                let (codex_sessions, next) = codex.unwrap_or_default();
                let mut sessions: Vec<Session> = codex_sessions
                    .into_iter()
                    .chain(claude.unwrap_or_default())
                    .collect();
                sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at));
                let next = next.map(|codex| {
                    URL_SAFE_NO_PAD.encode(
                        serde_json::to_vec(&Cursor { codex: Some(codex) })
                            .expect("cursor serialization"),
                    )
                });
                Ok(json!({"sessions":sessions,"nextCursor":next,"warnings":warnings}))
            }
            Action::Create { cwd, agent } => value(match agent {
                Agent::Claude => self.claude.create(cwd)?,
                Agent::Codex => self.codex.create(cwd).await?,
            }),
            Action::Read { session_id } => {
                let (agent, id) = split_session(&session_id)?;
                value(
                    match agent {
                        Agent::Claude => self.claude.read(id).await?,
                        Agent::Codex => self.codex.read(id).await?,
                    }
                    .bounded(),
                )
            }
            Action::Send {
                session_id,
                text,
                model,
            } => {
                if text.trim().is_empty()
                    || text.chars().count() > 32_000
                    || model
                        .as_ref()
                        .is_some_and(|model| model.is_empty() || model.len() > 200)
                {
                    return Err(Failure::new(400, "Invalid message or model."));
                }
                let (agent, id) = split_session(&session_id)?;
                match agent {
                    Agent::Claude => self.claude.send(id, text, model).await,
                    Agent::Codex => self.codex.send(id, text, model).await,
                }
            }
            Action::Approve {
                session_id,
                approval_id,
                decision,
            } => {
                let (agent, id) = split_session(&session_id)?;
                match agent {
                    Agent::Claude => self.claude.approve(id, approval_id, decision).await,
                    Agent::Codex => self.codex.approve(id, approval_id, decision).await,
                }
            }
            Action::Interrupt { session_id } => {
                let (agent, id) = split_session(&session_id)?;
                match agent {
                    Agent::Claude => self.claude.interrupt(id).await,
                    Agent::Codex => self.codex.interrupt(id).await,
                }
            }
        }
    }
}
