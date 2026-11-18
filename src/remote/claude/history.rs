use super::super::protocol::{redact, Failure, Message, Result};
use super::{visible, Claude, Grant};
use serde_json::{json, Value};
use std::{
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct Transcript {
    pub(super) messages: Vec<Message>,
    block: String,
    text_blocks: usize,
    older: bool,
}

impl Transcript {
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
            self.older = true;
        }
    }

    pub(super) fn append(&mut self, value: &Value) {
        let role = value["type"].as_str().unwrap_or("");
        let id = value["uuid"].as_str().unwrap_or("");
        let block = value["message"]["id"].as_str().unwrap_or(id);
        if role == "assistant" && self.block != block {
            self.block = block.into();
            self.text_blocks = 0;
        }
        for mut message in visible(&value["message"], id, role) {
            if role == "assistant" && message.role == "assistant" {
                message.id = format!("{block}/{}", self.text_blocks);
                self.text_blocks += 1;
            }
            self.message(message, false);
        }
    }

    pub(super) fn stream(&mut self, event: &Value) {
        if event["type"] == "message_start" {
            self.block = event["message"]["id"].as_str().unwrap_or("").into();
            self.text_blocks = 0;
        }
        if event["type"] == "content_block_delta" && event["delta"]["type"] == "text_delta" {
            self.message(
                Message {
                    id: format!("{}/{}", self.block, self.text_blocks),
                    role: "assistant".into(),
                    text: event["delta"]["text"].as_str().unwrap_or("").into(),
                },
                true,
            );
        }
    }
}

impl Claude {
    pub(super) fn grants(&self) -> Result<Vec<Grant>> {
        let entries = match std::fs::read_dir(&self.grants) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(Failure::new(503, "Cannot read Claude handoff records.")),
        };
        entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .map(|path| {
                std::fs::read(&path)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .ok_or_else(|| Failure::new(503, "A Claude handoff record is unreadable."))
            })
            .collect()
    }

    pub(super) fn grant(&self, id: Uuid) -> Result<Grant> {
        self.grants()?.into_iter().find(|grant| grant.id == id).ok_or_else(|| Failure::new(409, "Finish the current turn, run /exit, then run relay remote handoff with the UUID from /status on your Mac."))
    }

    pub(super) fn save(&self, grant: &Grant) -> Result<()> {
        crate::system::write_private(
            &self.grants.join(format!("{}.json", grant.id)),
            &serde_json::to_string(grant)
                .map_err(|_| Failure::new(500, "Cannot encode handoff record."))?,
        )
        .map_err(|_| Failure::new(503, "Cannot save Claude handoff record."))
    }

    pub(super) fn transcript(&self, id: Uuid) -> Result<PathBuf> {
        let entries = std::fs::read_dir(self.home.join("projects"))
            .map_err(|_| Failure::new(404, "Claude session not found."))?;
        entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path().join(format!("{id}.jsonl")))
            .find(|path| path.is_file())
            .ok_or_else(|| Failure::new(404, "Claude session not found."))
    }

    pub(super) fn history(&self, id: Uuid) -> Result<(String, Vec<Message>, bool)> {
        let path = self.transcript(id)?;
        let mut file = std::fs::File::open(path)
            .map_err(|_| Failure::new(503, "Cannot read Claude history."))?;
        let size = file
            .metadata()
            .map_err(|_| Failure::new(503, "Cannot read Claude history."))?
            .len();
        let mut prefix = Vec::new();
        Read::by_ref(&mut file)
            .take(65_536)
            .read_to_end(&mut prefix)
            .map_err(|_| Failure::new(503, "Cannot read Claude history."))?;
        let cwd = prefix
            .split(|byte| *byte == b'\n')
            .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
            .find_map(|value| value["cwd"].as_str().map(str::to_owned))
            .ok_or_else(|| Failure::new(502, "Claude transcript has no project folder."))?;
        let offset = size.saturating_sub(4 * 1024 * 1024);
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| Failure::new(503, "Cannot read Claude history."))?;
        let mut bytes = Vec::new();
        file.take(4 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|_| Failure::new(503, "Cannot read Claude history."))?;
        let transcript = bytes
            .split(|byte| *byte == b'\n')
            .skip(usize::from(offset > 0))
            .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
            .filter(|value| value["parent_tool_use_id"].is_null() && value["isMeta"] != true)
            .fold(Transcript::default(), |mut transcript, value| {
                transcript.append(&value);
                transcript
            });
        Ok((cwd, transcript.messages, offset > 0 || transcript.older))
    }
}

pub(super) fn settings(home: &Path, cwd: &Path) -> Result<Value> {
    let mut effective = json!({"env":{}});
    let mut paths = vec![home.join("settings.json")];
    let mut ancestors = cwd.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    for directory in ancestors {
        paths.push(directory.join(".claude/settings.json"));
        paths.push(directory.join(".claude/settings.local.json"));
    }
    let managed = if cfg!(target_os = "macos") {
        Path::new("/Library/Application Support/ClaudeCode")
    } else {
        Path::new("/etc/claude-code")
    };
    paths.push(managed.join("managed-settings.json"));
    if let Ok(entries) = std::fs::read_dir(managed.join("managed-settings.d")) {
        let mut fragments = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>();
        fragments.sort();
        paths.extend(fragments);
    }
    for path in paths {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(Failure::new(503, "Cannot verify Claude settings.")),
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
            Failure::new(
                409,
                "Fix invalid Claude settings on your Mac before connecting remotely.",
            )
        })?;
        if let Some(env) = value["env"].as_object() {
            for (key, value) in env {
                effective["env"][key] = value.clone();
            }
        }
        if let Some(helper) = value.get("apiKeyHelper") {
            effective["apiKeyHelper"] = helper.clone();
        }
    }
    Ok(effective)
}
