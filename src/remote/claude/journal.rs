use super::super::protocol::{now_ms, ExpiredApproval, Failure, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
};
use uuid::Uuid;

const MAX_AGE: i64 = 7 * 24 * 60 * 60 * 1000;
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct Journal {
    directory: PathBuf,
    home: PathBuf,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    session: Uuid,
    turn: String,
    id: String,
    request: String,
    tool: String,
    summary: String,
    state: State,
    created_at: i64,
    transcript_len: u64,
}

#[derive(Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum State {
    Pending,
    Allowed,
    Declined,
}

impl Journal {
    pub(super) fn new(directory: PathBuf, home: PathBuf) -> Self {
        Self { directory, home }
    }

    fn path(&self, session: Uuid) -> PathBuf {
        self.directory.join(format!("{session}.json"))
    }

    fn transcript(&self, session: Uuid) -> Option<PathBuf> {
        fs::read_dir(self.home.join("projects"))
            .ok()?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path().join(format!("{session}.jsonl")))
            .find(|path| path.is_file())
    }

    fn records(&self, session: Uuid) -> Vec<Record> {
        fs::File::open(self.path(session))
            .ok()
            .filter(|file| file.metadata().is_ok_and(|meta| meta.len() <= MAX_BYTES))
            .and_then(|file| serde_json::from_reader::<_, Vec<Record>>(file.take(MAX_BYTES)).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|record| record.session == session && record.created_at >= now_ms() - MAX_AGE)
            .take(32)
            .collect()
    }

    fn write(&self, session: Uuid, records: &[Record]) -> Result<()> {
        let save = || -> std::io::Result<()> {
            fs::create_dir_all(&self.directory)?;
            #[cfg(unix)]
            if let Some(parent) = self.directory.parent() {
                fs::File::open(parent)?.sync_all()?;
            }
            let staged = self
                .directory
                .join(format!(".approval-{}.tmp", Uuid::new_v4()));
            let result = (|| -> std::io::Result<()> {
                let mut file = crate::system::create_private(&staged)?;
                let contents = serde_json::to_vec(records)?;
                if contents.len() as u64 > MAX_BYTES {
                    return Err(std::io::Error::other(
                        "Claude approval journal is too large",
                    ));
                }
                file.write_all(&contents)?;
                file.flush()?;
                file.sync_all()?;
                fs::rename(&staged, self.path(session))?;
                #[cfg(unix)]
                fs::File::open(&self.directory)?.sync_all()?;
                Ok(())
            })();
            if result.is_err() {
                let _ = fs::remove_file(staged);
            }
            result
        };
        save().map_err(|_| {
            Failure::new(
                503,
                "Cannot safely save Claude approval state. The decision was not sent.",
            )
        })
    }

    fn prune(&self, current: Uuid) {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return;
        };
        let mut files = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.strip_suffix(".json")
                        .is_some_and(|stem| Uuid::parse_str(stem).is_ok())
                        || name.starts_with(".approval-") && name.ends_with(".tmp")
                })
            })
            .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
            .collect::<Vec<_>>();
        files.sort_by_key(|(modified, _)| *modified);
        let excess = files.len().saturating_sub(255);
        for (index, (modified, path)) in files.into_iter().enumerate() {
            if path != self.path(current)
                && (index < excess
                    || modified
                        .elapsed()
                        .is_ok_and(|age| age.as_millis() > MAX_AGE as u128))
            {
                let _ = fs::remove_file(path);
            }
        }
    }

    pub(super) fn pending(
        &self,
        session: Uuid,
        turn: &str,
        request_id: &str,
        request: &Value,
        summary: &str,
    ) -> Result<()> {
        self.prune(session);
        let mut records = self.records(session);
        records.retain(|record| record.request != request_id);
        if records.len() >= 32 {
            return Err(Failure::new(
                503,
                "Too many Claude approval records. Stop the turn before retrying.",
            ));
        }
        records.push(Record {
            session,
            turn: turn.into(),
            id: request["tool_use_id"]
                .as_str()
                .ok_or_else(|| Failure::new(502, "Claude approval has no tool use id."))?
                .into(),
            request: request_id.into(),
            tool: request["tool_name"].as_str().unwrap_or("Tool").into(),
            summary: summary.into(),
            state: State::Pending,
            created_at: now_ms(),
            transcript_len: self
                .transcript(session)
                .and_then(|path| fs::metadata(path).ok())
                .map_or(0, |meta| meta.len()),
        });
        self.write(session, &records)
    }

    pub(super) fn decide(&self, session: Uuid, request: &str, allowed: bool) -> Result<()> {
        let mut records = self.records(session);
        let record = records
            .iter_mut()
            .find(|record| record.request == request && record.state == State::Pending)
            .ok_or_else(|| {
                Failure::new(
                    409,
                    "This approval can no longer be safely answered. Stop the turn and retry.",
                )
            })?;
        record.state = if allowed {
            State::Allowed
        } else {
            State::Declined
        };
        self.write(session, &records)
    }

    pub(super) fn cancel(&self, session: Uuid, request: &str) -> Result<()> {
        let mut records = self.records(session);
        records.retain(|record| record.request != request);
        self.write(session, &records)
    }

    pub(super) fn clear(&self, session: Uuid) -> Result<()> {
        let clear = || -> std::io::Result<()> {
            match fs::remove_file(self.path(session)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            }
            #[cfg(unix)]
            fs::File::open(&self.directory)?.sync_all()?;
            Ok(())
        };
        clear().map_err(|_| Failure::new(503, "Cannot safely clear Claude approval state."))
    }

    fn later_turn(&self, record: &Record) -> bool {
        let Some(path) = self.transcript(record.session) else {
            return false;
        };
        let read = || -> std::io::Result<bool> {
            let mut file = fs::File::open(path)?;
            let length = file.metadata()?.len();
            if length < record.transcript_len || length - record.transcript_len > MAX_BYTES {
                return Ok(true);
            }
            file.seek(SeekFrom::Start(record.transcript_len))?;
            let mut bytes = Vec::new();
            file.take(MAX_BYTES).read_to_end(&mut bytes)?;
            Ok(bytes
                .split(|byte| *byte == b'\n')
                .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
                .any(|value| {
                    value["type"] == "user"
                        && value["isMeta"] != true
                        && value["parent_tool_use_id"].is_null()
                        && value["uuid"] != record.turn
                        && (value["message"]["content"].is_string()
                            || value["message"]["content"]
                                .as_array()
                                .is_some_and(|blocks| {
                                    blocks.iter().any(|block| block["type"] == "text")
                                }))
                }))
        };
        read().unwrap_or(true)
    }

    pub(super) fn expired(&self, session: Uuid) -> Option<ExpiredApproval> {
        self.records(session)
            .into_iter()
            .rev()
            .find(|record| record.state == State::Pending && !self.later_turn(record))
            .map(|record| ExpiredApproval {
                id: record.id,
                tool: record.tool,
                summary: record.summary,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn oversized_updates_preserve_the_last_durable_pending_record() {
        let directory = std::env::temp_dir().join(format!("claude-journal-{}", Uuid::new_v4()));
        let session = Uuid::new_v4();
        let journal = Journal::new(directory.join("approvals"), directory.join("config"));
        let request = json!({"tool_use_id":"tool-1","tool_name":"Bash"});
        journal
            .pending(session, "turn-1", "request-1", &request, "Bash\necho ok")
            .unwrap();
        assert!(journal
            .pending(
                session,
                "turn-1",
                "request-2",
                &request,
                &"x".repeat(MAX_BYTES as usize)
            )
            .is_err());
        let restored = Journal::new(directory.join("approvals"), directory.join("config"));
        assert_eq!(restored.expired(session).unwrap().summary, "Bash\necho ok");
        assert_eq!(
            fs::read_dir(directory.join("approvals")).unwrap().count(),
            1
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
