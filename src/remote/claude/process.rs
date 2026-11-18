use super::super::{
    protocol::{now_ms, public_error, redact, Approval, Failure, Message, Result},
    route,
};
use super::history::settings;
use super::{visible, Claude, Grant};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{mpsc, oneshot},
};
use url::Url;
use uuid::Uuid;

pub(super) struct Permission {
    pub(super) request: String,
    pub(super) input: Value,
    pub(super) approval: Approval,
}
#[derive(Default)]
pub(super) struct Live {
    pub(super) working: bool,
    pub(super) closed: bool,
    pub(super) stopping: bool,
    pub(super) model: Option<String>,
    pub(super) error: Option<String>,
    pub(super) messages: Vec<Message>,
    pub(super) approvals: HashMap<Uuid, Permission>,
    pub(super) block: String,
}

impl Live {
    pub(super) fn message(&mut self, message: Message, append: bool) {
        if let Some(previous) = self
            .messages
            .iter_mut()
            .find(|value| value.id == message.id)
        {
            previous.text = if append {
                format!("{}{}", previous.text, message.text)
            } else {
                message.text
            };
            previous.text = redact(&previous.text).chars().take(256_000).collect();
            return;
        }
        self.messages.push(Message {
            text: redact(&message.text).chars().take(256_000).collect(),
            ..message
        });
        if self.messages.len() > 120 {
            self.messages.remove(0);
        }
    }

    fn event(&mut self, value: &Value, id: Uuid) -> Result<Option<Value>> {
        match value["type"].as_str().unwrap_or("") {
            "system" if value["subtype"] == "init" => {
                if value["session_id"] != id.to_string() {
                    return Err(Failure::new(502, "Claude opened a different session. Relay stopped it to avoid sending into a copy."));
                }
                self.model = value["model"].as_str().map(str::to_owned);
            }
            "assistant" | "user" if value["parent_tool_use_id"].is_null() => {
                for message in visible(
                    &value["message"],
                    value["uuid"].as_str().unwrap_or(""),
                    value["type"].as_str().unwrap_or(""),
                ) {
                    self.message(message, false);
                }
            }
            "stream_event" if value["parent_tool_use_id"].is_null() => {
                let event = &value["event"];
                if event["type"] == "message_start" {
                    self.block = event["message"]["id"].as_str().unwrap_or("").into();
                }
                if event["type"] == "content_block_delta" && event["delta"]["type"] == "text_delta"
                {
                    self.message(
                        Message {
                            id: format!("{}/{}", self.block, event["index"]),
                            role: "assistant".into(),
                            text: event["delta"]["text"].as_str().unwrap_or("").into(),
                        },
                        true,
                    );
                }
            }
            "result" => {
                self.working = false;
                self.approvals.clear();
                if !self.stopping && (value["is_error"] == true || value["subtype"] != "success") {
                    self.error = Some(public_error(value["result"].as_str().unwrap_or_else(
                        || {
                            value["errors"][0]
                                .as_str()
                                .unwrap_or("Claude could not complete this turn.")
                        },
                    )));
                }
                self.stopping = false;
            }
            "control_request" => {
                let request = &value["request"];
                let request_id = value["request_id"]
                    .as_str()
                    .ok_or_else(|| Failure::new(502, "Invalid Claude permission request."))?;
                if request["subtype"] != "can_use_tool" {
                    return Ok(Some(
                        json!({"type":"control_response","response":{"subtype":"error","request_id":request_id,"error":"Review this request on your Mac."}}),
                    ));
                }
                let tool = request["tool_name"].as_str().unwrap_or("Tool");
                let full = format!(
                    "{tool}\n{}",
                    serde_json::to_string_pretty(&request["input"]).unwrap_or_default()
                );
                let approval = Uuid::new_v4();
                self.approvals.insert(
                    approval,
                    Permission {
                        request: request_id.into(),
                        input: request["input"].clone(),
                        approval: Approval {
                            id: approval.to_string(),
                            title: format!("Allow {tool}?"),
                            text: redact(&full).chars().take(32_000).collect(),
                            can_accept: full.len() <= 32_000
                                && !matches!(tool, "AskUserQuestion" | "ExitPlanMode"),
                        },
                    },
                );
            }
            "control_cancel_request" => self.approvals.retain(|_, approval| {
                approval.request != value["request_id"].as_str().unwrap_or("")
            }),
            _ => {}
        }
        Ok(None)
    }
}

struct Write {
    pub(super) value: Value,
    pub(super) control: bool,
    pub(super) reply: oneshot::Sender<Result<Value>>,
}
#[derive(Clone)]
pub(super) struct Running {
    sender: mpsc::Sender<Write>,
    pub(super) live: Arc<Mutex<Live>>,
    pub(super) endpoint: Url,
}

impl Running {
    pub(super) async fn write(&self, value: Value, control: bool) -> Result<Value> {
        let (reply, receive) = oneshot::channel();
        self.sender
            .send(Write {
                value,
                control,
                reply,
            })
            .await
            .map_err(|_| Failure::new(503, "Claude disconnected."))?;
        tokio::time::timeout(Duration::from_secs(15), receive)
            .await
            .map_err(|_| {
                Failure::new(
                    504,
                    "Claude did not confirm the request. Refresh before retrying.",
                )
            })?
            .map_err(|_| Failure::new(503, "Claude disconnected."))?
    }
    pub(super) async fn control(&self, request: Value) -> Result<Value> {
        self.write(json!({"type":"control_request","request_id":Uuid::new_v4().to_string(),"request":request}), true).await
    }
}

impl Claude {
    pub(super) async fn active(&self) -> Result<Vec<Grant>> {
        let mut command = Command::new(&self.executable);
        command
            .args(["agents", "--json"])
            .env("CLAUDE_CONFIG_DIR", &self.home);
        let bytes = output(command).await?;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Active {
            session_id: Option<Uuid>,
            cwd: String,
            name: Option<String>,
            pid: Option<u32>,
        }
        let active: Vec<Active> = serde_json::from_slice(&bytes).map_err(|_| {
            Failure::new(
                502,
                "This Claude version does not support session discovery.",
            )
        })?;
        Ok(active
            .into_iter()
            .filter(|session| session.pid.is_some())
            .filter_map(|session| {
                Some(Grant {
                    id: session.session_id?,
                    cwd: session.cwd,
                    title: session.name.unwrap_or_else(|| "Claude session".into()),
                    model: None,
                    updated_at: now_ms() / 1000,
                })
            })
            .collect())
    }

    pub(super) async fn connection(&self, grant: &Grant) -> Result<(Url, bool)> {
        let settings = settings(&self.home, Path::new(&grant.cwd))?;
        let endpoint = settings["env"]["ANTHROPIC_BASE_URL"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
            .and_then(|value| Url::parse(&value).ok())
            .ok_or_else(|| {
                Failure::new(
                    409,
                    "Configure Claude Code with Relay before connecting remotely.",
                )
            })?;
        if [
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        ]
        .iter()
        .any(|key| {
            settings["env"][key].as_str() == Some("1") || std::env::var(key).as_deref() == Ok("1")
        }) {
            return Err(Failure::new(
                409,
                "This Claude project selects a different provider.",
            ));
        }
        let local = route::verify(&endpoint, &self.gateway).await?;
        if local
            && !settings["apiKeyHelper"]
                .as_str()
                .is_some_and(|helper| !helper.is_empty())
        {
            return Err(Failure::new(
                409,
                "Run relay onboard on this Mac to configure Claude’s proxy credential helper.",
            ));
        }
        if local
            && [
                "LITELLM_API_KEY",
                "ANTHROPIC_API_KEY",
                "ANTHROPIC_AUTH_TOKEN",
            ]
            .iter()
            .any(|key| {
                settings["env"][key]
                    .as_str()
                    .is_some_and(|value| !value.is_empty())
            })
        {
            return Err(Failure::new(409, "Remove embedded API keys from Claude settings and use Relay’s proxy credential helper."));
        }
        Ok((endpoint, local))
    }

    pub(super) async fn start(&self, grant: &Grant) -> Result<Running> {
        if let Some(run) = self
            .runs
            .lock()
            .expect("Claude runs lock")
            .get(&grant.id)
            .filter(|run| !run.live.lock().expect("Claude live lock").closed)
        {
            return Ok(run.clone());
        }
        if self
            .active()
            .await?
            .iter()
            .any(|session| session.id == grant.id)
        {
            return Err(Failure::new(
                409,
                "This session is open outside Relay. Exit it on your Mac first.",
            ));
        }
        if self
            .runs
            .lock()
            .expect("Claude runs lock")
            .values()
            .filter(|run| !run.live.lock().expect("Claude live lock").closed)
            .count()
            >= 4
        {
            return Err(Failure::new(429, "Four Claude sessions are already open. Restart the connector to close idle sessions."));
        }
        crate::ai_tools::claude_cli::refuse_node_script_claude_code(Some(&self.executable))
            .map_err(|_| {
                Failure::new(
                    409,
                    "Install native Claude Code so Relay can verify its signature.",
                )
            })?;
        let (endpoint, local) = self.connection(grant).await?;
        let resume = self.transcript(grant.id).is_ok();
        let mut command = Command::new(&self.executable);
        command
            .current_dir(&grant.cwd)
            .args([
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--permission-prompt-tool",
                "stdio",
                "--permission-mode",
                "default",
                "--setting-sources",
                "user,project,local",
            ])
            .arg(if resume {
                format!("--resume={}", grant.id)
            } else {
                format!("--session-id={}", grant.id)
            })
            .env("ANTHROPIC_BASE_URL", endpoint.as_str())
            .env("CLAUDE_CONFIG_DIR", &self.home)
            .env("DISABLE_AUTOUPDATER", "1")
            .env_remove("CLAUDECODE")
            .env_remove("CLAUDE_CODE_USE_BEDROCK")
            .env_remove("CLAUDE_CODE_USE_VERTEX")
            .env_remove("CLAUDE_CODE_USE_FOUNDRY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if local {
            command
                .env_remove("LITELLM_API_KEY")
                .env_remove("ANTHROPIC_API_KEY")
                .env_remove("ANTHROPIC_AUTH_TOKEN");
        } else if let Ok(key) = std::env::var("LITELLM_API_KEY") {
            command
                .env_remove("ANTHROPIC_API_KEY")
                .env("ANTHROPIC_AUTH_TOKEN", key);
        }
        if let Some(model) = &grant.model {
            command.arg("--model").arg(model);
        }
        let mut child = command
            .spawn()
            .map_err(|_| Failure::new(503, "Cannot start native Claude Code."))?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| Failure::new(503, "Cannot open Claude input."))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| Failure::new(503, "Cannot read Claude output."))?;
        let (sender, mut receiver) = mpsc::channel::<Write>(32);
        let live = Arc::new(Mutex::new(Live::default()));
        let state = live.clone();
        let id = grant.id;
        tokio::spawn(async move {
            let mut reader = BufReader::new(output).take(4 * 1024 * 1024 + 1);
            let mut pending = HashMap::<String, oneshot::Sender<Result<Value>>>::new();
            let mut line = Vec::new();
            loop {
                tokio::select! {
                    write = receiver.recv() => {
                        let Some(write) = write else { break; };
                        if input.write_all(format!("{}\n", write.value).as_bytes()).await.is_err() { let _ = write.reply.send(Err(Failure::new(503, "Claude disconnected."))); break; }
                        if write.control { pending.retain(|_, reply| !reply.is_closed()); pending.insert(write.value["request_id"].as_str().unwrap_or("").into(), write.reply); }
                        else { let _ = write.reply.send(Ok(json!({}))); }
                    }
                    read = reader.read_until(b'\n', &mut line) => {
                        if !matches!(read, Ok(1..)) || line.len() > 4 * 1024 * 1024 || !line.ends_with(b"\n") { break; }
                        let Ok(value) = serde_json::from_slice::<Value>(&line) else { break; };
                        line.clear();
                        reader.set_limit(4 * 1024 * 1024 + 1);
                        if value["type"] == "control_response" {
                            let response = &value["response"];
                            if let Some(reply) = response["request_id"].as_str().and_then(|id| pending.remove(id)) {
                                let result = if response["subtype"] == "success" { Ok(response["response"].clone()) } else { Err(Failure::new(502, public_error(response["error"].as_str().unwrap_or("Claude rejected the request.")))) };
                                let _ = reply.send(result);
                            }
                            continue;
                        }
                        let event = state.lock().expect("Claude live lock").event(&value, id);
                        match event {
                            Ok(Some(response)) => { if input.write_all(format!("{response}\n").as_bytes()).await.is_err() { break; } }
                            Ok(None) => {}
                            Err(error) => { state.lock().expect("Claude live lock").error = Some(error.error); break; }
                        }
                    }
                }
            }
            let _ = child.kill().await;
            let mut live = state.lock().expect("Claude live lock");
            if live.working && !live.stopping && live.error.is_none() {
                live.error = Some("Claude disconnected before confirming the turn completed. Refresh before retrying.".into());
            }
            live.closed = true;
            live.working = false;
            live.approvals.clear();
        });
        let run = Running {
            sender,
            live,
            endpoint,
        };
        run.control(json!({"subtype":"initialize","hooks":null}))
            .await?;
        self.runs
            .lock()
            .expect("Claude runs lock")
            .insert(grant.id, run.clone());
        Ok(run)
    }
}

async fn output(mut command: Command) -> Result<Vec<u8>> {
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|_| {
        Failure::new(
            503,
            "Install native Claude Code and configure Relay on this Mac.",
        )
    })?;
    let mut bytes = Vec::new();
    let read = async {
        child
            .stdout
            .take()
            .ok_or_else(|| Failure::new(503, "Cannot read Claude output."))?
            .take(1_048_577)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| Failure::new(503, "Cannot read Claude output."))?;
        if bytes.len() > 1_048_576 {
            return Err(Failure::new(
                502,
                "Claude discovery returned too much data.",
            ));
        }
        let status = child
            .wait()
            .await
            .map_err(|_| Failure::new(503, "Cannot check Claude sessions."))?;
        if !status.success() {
            return Err(Failure::new(503, "Cannot check running Claude sessions."));
        }
        Ok(())
    };
    tokio::time::timeout(Duration::from_secs(5), read)
        .await
        .map_err(|_| Failure::new(504, "Claude session discovery timed out."))??;
    Ok(bytes)
}
