use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;

use crate::http::{is_sensitive_name, scrub_request_target};

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
    redact_value(&mut event);
    if let Value::Object(map) = &mut event {
        map.entry("captured_at")
            .or_insert_with(|| Value::String(Utc::now().to_rfc3339()));
    }
    event
}

fn redact_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                if should_remove_field(&key) {
                    map.remove(&key);
                    continue;
                }
                if is_sensitive_name(&key) {
                    map.insert(key, Value::String("<redacted>".into()));
                    continue;
                }
                if let Some(child) = map.get_mut(&key) {
                    if matches!(key.as_str(), "path" | "url" | "target") {
                        if let Value::String(raw) = child {
                            *raw = scrub_request_target(raw);
                            continue;
                        }
                    }
                    redact_value(child);
                }
            }
        }
        Value::Array(values) => {
            for child in values {
                redact_value(child);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn should_remove_field(key: &str) -> bool {
    matches!(key.to_ascii_lowercase().as_str(), "body" | "prompt")
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
        assert_eq!(redacted["authorization"], "<redacted>");
        assert_eq!(redacted["cookie"], "<redacted>");
        assert!(redacted.get("body").is_none());
        assert_eq!(redacted["host"], "www.notion.so");
    }

    #[test]
    fn redact_event_scrubs_nested_headers_and_query_secrets() {
        let redacted = redact_event(json!({
            "event": "http_request",
            "path": "/v1/responses?api_key=sk-secret&model=gpt-4o",
            "headers": {
                "x-api-key": "secret",
                "content-type": "application/json"
            },
            "metadata": {
                "session_id": "abc123"
            }
        }));

        assert_eq!(
            redacted["path"],
            "/v1/responses?api_key=%3Credacted%3E&model=gpt-4o"
        );
        assert_eq!(redacted["headers"]["x-api-key"], "<redacted>");
        assert_eq!(redacted["headers"]["content-type"], "application/json");
        assert_eq!(redacted["metadata"]["session_id"], "<redacted>");
    }
}
