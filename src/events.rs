use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

pub fn append_event(log_path: &Path, event: Value) -> Result<()> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let event = redact_event(event);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)?;
    writeln!(file, "{}", serde_json::to_string(&event)?)?;
    Ok(())
}

pub fn clear_events(log_path: &Path) -> Result<()> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(log_path, "")?;
    Ok(())
}

pub fn read_events(log_path: &Path, limit: usize) -> Vec<Value> {
    let Ok(contents) = fs::read_to_string(log_path) else {
        return vec![];
    };
    let mut events = VecDeque::with_capacity(limit);
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(line) {
            if events.len() == limit {
                events.pop_front();
            }
            events.push_back(Value::Object(map));
        }
    }
    events.into_iter().collect()
}

pub fn redact_event(mut event: Value) -> Value {
    if let Value::Object(map) = &mut event {
        map.entry("captured_at")
            .or_insert_with(|| Value::String(Utc::now().to_rfc3339()));
        for key in [
            "authorization",
            "cookie",
            "token_v2",
            "prompt",
            "body",
            "url",
        ] {
            map.remove(key);
        }
    }
    event
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn redact_event_drops_sensitive_fields() {
        let redacted = redact_event(json!({
            "event": "connect",
            "host": "www.notion.so",
            "authorization": "Bearer secret",
            "cookie": "token_v2=secret",
            "body": "prompt",
        }));
        assert!(redacted.get("authorization").is_none());
        assert!(redacted.get("cookie").is_none());
        assert!(redacted.get("body").is_none());
        assert_eq!(redacted["host"], "www.notion.so");
    }
}
