use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
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

const TAIL_WINDOW_BYTES: u64 = 256 * 1024;

pub fn read_events(log_path: &Path, limit: usize) -> Vec<Value> {
    let Ok(mut file) = File::open(log_path) else {
        return vec![];
    };
    let Ok(length) = file.metadata().map(|metadata| metadata.len()) else {
        return vec![];
    };
    let mut window = TAIL_WINDOW_BYTES;
    loop {
        let start = length.saturating_sub(window);
        let Some(events) = newest_events_from(&mut file, start, limit) else {
            return vec![];
        };
        if start == 0 || events.len() == limit {
            return events;
        }
        window = window.saturating_mul(2);
    }
}

fn newest_events_from(file: &mut File, start: u64, limit: usize) -> Option<Vec<Value>> {
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let whole_lines = match start {
        0 => &bytes[..],
        _ => bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(&[][..], |newline| &bytes[newline + 1..]),
    };
    let newest_first: Vec<Value> = whole_lines
        .rsplit(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter(Value::is_object)
        .take(limit)
        .collect();
    Some(newest_first.into_iter().rev().collect())
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
    fn read_events_returns_the_newest_events_in_order_from_a_log_larger_than_the_tail_window() {
        let log_path = std::env::temp_dir().join(format!(
            "relay-events-tail-{}.log.jsonl",
            uuid::Uuid::new_v4()
        ));
        let padding = "x".repeat(4096);
        let append = |index: u64| {
            append_event(
                &log_path,
                json!({"event": "connect", "index": index, "pad": padding}),
            )
            .unwrap()
        };
        for index in 0..300 {
            append(index);
            if index % 50 == 0 {
                OpenOptions::new()
                    .append(true)
                    .open(&log_path)
                    .and_then(|mut file| file.write_all(b"not json\n[1, 2]\n\n"))
                    .unwrap();
            }
        }
        let indexes = |events: Vec<Value>| {
            events
                .iter()
                .map(|event| event["index"].as_u64().unwrap())
                .collect::<Vec<_>>()
        };

        let newest = indexes(read_events(&log_path, 100));
        let every = indexes(read_events(&log_path, 1000));
        append(300);
        let after_append = indexes(read_events(&log_path, 100));
        fs::remove_file(&log_path).unwrap();

        assert_eq!(newest, (200..300).collect::<Vec<_>>());
        assert_eq!(every, (0..300).collect::<Vec<_>>());
        assert_eq!(after_append, (201..301).collect::<Vec<_>>());
    }

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
