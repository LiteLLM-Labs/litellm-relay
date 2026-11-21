mod backend;
mod history;
mod journal;
mod process;
pub use backend::Backend;
pub(super) mod supervisor;

use super::protocol::{now_ms, redact, Agent, Decision, Detail, Failure, Message, Result, Session};
use process::Running;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Mutex};
use tokio::sync::Mutex as AsyncMutex;
use url::Url;
use uuid::Uuid;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Grant {
    id: Uuid,
    cwd: String,
    title: String,
    model: Option<String>,
    updated_at: i64,
}

pub struct Claude {
    executable: PathBuf,
    home: PathBuf,
    grants: PathBuf,
    conversations: Mutex<HashMap<Uuid, Grant>>,
    gateway: Url,
    runs: Mutex<HashMap<Uuid, Running>>,
    mutation: AsyncMutex<()>,
}

impl Claude {
    pub(super) fn has_open_run(&self, id: Uuid) -> bool {
        self.runs
            .lock()
            .expect("Claude runs lock")
            .get(&id)
            .is_some_and(|run| !run.live.lock().expect("Claude live lock").closed)
    }

    pub(super) fn has_open_runs(&self) -> bool {
        self.runs
            .lock()
            .expect("Claude runs lock")
            .values()
            .any(|run| !run.live.lock().expect("Claude live lock").closed)
    }

    pub(super) async fn close_runs(&self) {
        let runs: Vec<_> = self
            .runs
            .lock()
            .expect("Claude runs lock")
            .values()
            .cloned()
            .collect();
        for run in runs {
            let _ = run.close().await;
        }
    }

    fn journal(&self) -> journal::Journal {
        journal::Journal::new(self.grants.join("approvals"), self.home.clone())
    }

    pub fn new(executable: PathBuf, home: PathBuf, grants: PathBuf, gateway: Url) -> Self {
        Self {
            executable,
            home,
            grants,
            conversations: Mutex::new(HashMap::new()),
            gateway,
            runs: Mutex::new(HashMap::new()),
            mutation: AsyncMutex::new(()),
        }
    }

    pub async fn handoff(&self, id: Uuid) -> Result<()> {
        if self.active().await?.iter().any(|grant| grant.id == id) {
            return Err(Failure::new(
                409,
                "This session is still open. Finish its turn and run /exit before handing it over.",
            ));
        }
        if self.grants()?.iter().any(|grant| grant.id == id) {
            return Ok(());
        }
        self.save(&self.conversation(id, self.transcript(id)?).await?)
    }

    fn summary(&self, grant: &Grant, external: bool) -> Session {
        let runs = self.runs.lock().expect("Claude runs lock");
        let live = runs
            .get(&grant.id)
            .map(|run| run.live.lock().expect("Claude live lock"));
        let working = live
            .as_ref()
            .is_some_and(|live| live.working || live.starting);
        Session {
            id: Agent::Claude.session_id(&grant.id.to_string()),
            agent: Agent::Claude,
            title: redact(&grant.title).chars().take(140).collect(),
            project: super::sessions::project(&grant.cwd),
            cwd: grant.cwd.clone(),
            model: live
                .as_ref()
                .and_then(|live| live.model.clone())
                .or_else(|| grant.model.clone()),
            status: if external {
                "handoff_required"
            } else if live.as_ref().is_some_and(|live| !live.approvals.is_empty()) {
                "needs_approval"
            } else if working {
                "working"
            } else if live.as_ref().is_some_and(|live| live.error.is_some()) {
                "error"
            } else {
                "ready"
            }
            .into(),
            can_send: !external && !working,
            can_stop: !external && working,
            updated_at: grant.updated_at,
        }
    }

    pub async fn list(&self) -> Result<Vec<Session>> {
        let grants = self.grants()?;
        let active = self.active().await?;
        let owned = self
            .runs
            .lock()
            .expect("Claude runs lock")
            .iter()
            .filter(|(_, run)| !run.live.lock().expect("Claude live lock").closed)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        Ok(grants
            .iter()
            .map(|grant| {
                self.summary(
                    grant,
                    !owned.contains(&grant.id)
                        && active.iter().any(|session| session.id == grant.id),
                )
            })
            .chain(
                active
                    .iter()
                    .filter(|grant| !grants.iter().any(|saved| saved.id == grant.id))
                    .map(|grant| self.summary(grant, true)),
            )
            .collect())
    }

    pub fn create(&self, cwd: String) -> Result<Session> {
        super::sessions::validate_directory(&cwd)?;
        let grant = Grant {
            id: Uuid::new_v4(),
            cwd,
            title: "New Claude session".into(),
            model: None,
            updated_at: now_ms() / 1000,
        };
        self.save(&grant)?;
        Ok(self.summary(&grant, false))
    }

    pub async fn read(&self, id: Uuid) -> Result<Detail> {
        let owned = self
            .runs
            .lock()
            .expect("Claude runs lock")
            .get(&id)
            .is_some_and(|run| !run.live.lock().expect("Claude live lock").closed);
        let external = if owned {
            None
        } else {
            self.active()
                .await?
                .into_iter()
                .find(|grant| grant.id == id)
        };
        let grant = self
            .grant(id)
            .or_else(|error| external.clone().ok_or(error))?;
        if external.is_some() {
            return Ok(Detail { session: self.summary(&grant, true), messages: Vec::new(), older_messages: false, approvals: Vec::new(), expired_approval: None, error: Some("On the device, finish this turn, run /exit, then run relay remote handoff and choose this conversation.".into()) });
        }
        let (stored, older) = match self.history(id) {
            Ok(history) => history,
            Err(error) if error.status == 404 => (Vec::new(), false),
            Err(error) => return Err(error),
        };
        let session = self.summary(&grant, false);
        let runs = self.runs.lock().expect("Claude runs lock");
        let live = runs
            .get(&id)
            .map(|run| run.live.lock().expect("Claude live lock"));
        let messages = stored
            .iter()
            .map(|message| {
                live.as_ref()
                    .and_then(|live| {
                        live.transcript.messages.iter().find(|value| {
                            value.id == message.id && value.text.len() > message.text.len()
                        })
                    })
                    .unwrap_or(message)
                    .clone()
            })
            .chain(
                live.as_ref()
                    .into_iter()
                    .flat_map(|live| live.transcript.messages.iter())
                    .filter(|message| !stored.iter().any(|value| value.id == message.id))
                    .cloned(),
            )
            .collect();
        Ok(Detail {
            session,
            expired_approval: if live
                .as_ref()
                .is_some_and(|live| live.working || live.starting)
            {
                None
            } else {
                self.journal().expired(id)
            },
            messages,
            older_messages: older,
            approvals: live
                .as_ref()
                .map(|live| {
                    live.approvals
                        .values()
                        .map(|permission| permission.approval.clone())
                        .collect()
                })
                .unwrap_or_default(),
            error: live.as_ref().and_then(|live| live.error.clone()),
        })
    }

    pub async fn send(&self, id: Uuid, text: String, model: Option<String>) -> Result<Value> {
        let _guard = self.mutation.lock().await;
        let grant = self.grant(id)?;
        let run = self.start(&grant, model.as_deref()).await?;
        if run.live.lock().expect("Claude live lock").working {
            return Err(Failure::new(
                409,
                "Wait for Claude to finish or stop the current turn.",
            ));
        }
        if self.connection(&grant).await?.0 != run.endpoint {
            return Err(Failure::new(
                409,
                "Relay’s endpoint changed. Restart the connector before continuing.",
            ));
        }
        if let Some(model) = &model {
            run.control(json!({"subtype":"set_model","model":model}))
                .await?;
        }
        let turn = Uuid::new_v4();
        {
            let mut live = run.live.lock().expect("Claude live lock");
            self.journal().clear(id)?;
            live.turn = turn.to_string();
            live.permission_closed = false;
            live.working = true;
            static USAGE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            live.last_used = USAGE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            live.error = None;
            live.transcript.message(
                Message {
                    id: turn.to_string(),
                    role: "user".into(),
                    text: text.clone(),
                },
                false,
            );
        }
        let sent = run.write(json!({"type":"user","session_id":id,"parent_tool_use_id":null,"uuid":turn,"message":{"role":"user","content":text}}), false).await;
        if let Err(error) = sent {
            let mut live = run.live.lock().expect("Claude live lock");
            live.working = false;
            live.error = Some(error.error.clone());
            return Err(error);
        }
        let updated = Grant {
            model: model.or(grant.model).or_else(|| run.startup_model.clone()),
            title: if grant.title == "New Claude session" {
                redact(text.lines().next().unwrap_or("Claude session"))
                    .chars()
                    .take(140)
                    .collect()
            } else {
                grant.title
            },
            updated_at: now_ms() / 1000,
            ..grant
        };
        if self.save(&updated).is_err() {
            eprintln!("A Claude message was sent but its local metadata could not be saved.");
        }
        Ok(json!({"turnId":turn}))
    }

    pub async fn approve(&self, id: Uuid, approval: Uuid, decision: Decision) -> Result<Value> {
        let _guard = self.mutation.lock().await;
        let run = self
            .runs
            .lock()
            .expect("Claude runs lock")
            .get(&id)
            .cloned()
            .ok_or_else(|| Failure::new(409, "This session is no longer running."))?;
        if decision == Decision::Accept
            && self.connection(&self.grant(id)?).await?.0 != run.endpoint
        {
            return Err(Failure::new(
                409,
                "Relay’s endpoint changed. Restart the connector before approving this request.",
            ));
        }
        let permission = {
            let mut live = run.live.lock().expect("Claude live lock");
            let permission = live
                .approvals
                .get(&approval)
                .ok_or_else(|| Failure::new(409, "This approval was already resolved."))?;
            if decision == Decision::Accept && !permission.approval.can_accept {
                return Err(Failure::new(400, "Review this request on the device."));
            }
            self.journal()
                .decide(id, &permission.request, decision == Decision::Accept)?;
            live.approvals
                .remove(&approval)
                .expect("pending permission")
        };
        let response = if decision == Decision::Accept {
            json!({"behavior":"allow","updatedInput":permission.input})
        } else {
            json!({"behavior":"deny","message":"The user declined this request."})
        };
        run.write(json!({"type":"control_response","response":{"subtype":"success","request_id":permission.request,"response":response}}), false).await?;
        Ok(json!({"resolved":true}))
    }

    pub async fn interrupt(&self, id: Uuid) -> Result<Value> {
        self.grant(id)?;
        let run = self
            .runs
            .lock()
            .expect("Claude runs lock")
            .get(&id)
            .cloned();
        let Some(run) = run.filter(|run| {
            let live = run.live.lock().expect("Claude live lock");
            live.working || live.starting
        }) else {
            return Ok(json!({"stopped":false}));
        };
        let starting = {
            let mut live = run.live.lock().expect("Claude live lock");
            self.journal().clear(id)?;
            live.stopping = true;
            live.approvals.clear();
            live.starting
        };
        if starting {
            run.close().await?;
            return Ok(json!({"stopped":true}));
        }
        run.control(json!({"subtype":"interrupt"})).await?;
        Ok(json!({"stopped":true}))
    }
}

pub fn visible(message: &Value, id: &str, role: &str) -> Vec<Message> {
    if !matches!(role, "user" | "assistant") {
        return Vec::new();
    }
    if let Some(text) = message["content"].as_str() {
        if role == "user" && command_wrapper(text) {
            return Vec::new();
        }
        return vec![Message {
            id: id.into(),
            role: role.into(),
            text: redact(text).chars().take(256_000).collect(),
        }];
    }
    let Some(blocks) = message["content"].as_array() else {
        return Vec::new();
    };
    if role == "user" {
        let text = blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        return if text.is_empty() || command_wrapper(&text) {
            Vec::new()
        } else {
            vec![Message {
                id: id.into(),
                role: role.into(),
                text: redact(&text).chars().take(256_000).collect(),
            }]
        };
    }
    blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            let key = format!("{}/{index}", message["id"].as_str().unwrap_or(id));
            match block["type"].as_str()? {
                "text" => Some(Message {
                    id: key,
                    role: role.into(),
                    text: redact(block["text"].as_str()?)
                        .chars()
                        .take(256_000)
                        .collect(),
                }),
                "tool_use" => Some(Message {
                    id: block["id"].as_str().unwrap_or(&key).into(),
                    role: "activity".into(),
                    text: redact(&format!(
                        "{}\n{}",
                        block["name"].as_str().unwrap_or("Tool"),
                        block["input"]
                    ))
                    .chars()
                    .take(32_000)
                    .collect(),
                }),
                _ => None,
            }
        })
        .collect()
}

fn command_wrapper(text: &str) -> bool {
    let text = text.trim();
    if text.starts_with("<local-command-stdout>") && text.ends_with("</local-command-stdout>") {
        return true;
    }
    let Some((command, rest)) = text
        .strip_prefix("<command-name>")
        .and_then(|text| text.split_once("</command-name>"))
    else {
        return false;
    };
    let Some((message, rest)) = rest
        .trim_start()
        .strip_prefix("<command-message>")
        .and_then(|text| text.split_once("</command-message>"))
    else {
        return false;
    };
    !message.is_empty()
        && command.strip_prefix('/') == Some(message)
        && rest
            .trim_start()
            .strip_prefix("<command-args>")
            .is_some_and(|args| args.ends_with("</command-args>"))
}
