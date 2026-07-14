use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{
    apps::AppAttribution, config::RelayConfig, system::hostname, traffic::TrafficClassification,
};

pub struct GatewayClient {
    config: Arc<RelayConfig>,
    http_client: reqwest::Client,
    last_shadow_by_host: Mutex<HashMap<String, Instant>>,
}

impl GatewayClient {
    pub fn new(config: Arc<RelayConfig>) -> Self {
        let timeout = Duration::from_secs_f64(config.request_timeout_seconds);
        let http_client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("reqwest client configuration should be valid");
        Self {
            config,
            http_client,
            last_shadow_by_host: Mutex::new(HashMap::new()),
        }
    }

    pub async fn maybe_shadow(&self, event: &Value) -> Value {
        if !self.config.shadow_enabled {
            return json!({"attempted": false, "ok": false});
        }
        let Some(api_key) = &self.config.gateway_api_key else {
            return json!({"attempted": false, "ok": false, "error": "gateway.api_key is not set in config.yaml"});
        };
        let host = event
            .get("host")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if self.shadow_is_throttled(host).await {
            return json!({"attempted": false, "ok": false, "error": "throttled"});
        }

        let event_id = event
            .get("event_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let payload = build_shadow_payload(event, &self.config, &event_id);
        let response = self
            .http_client
            .post(format!(
                "{}/v1/chat/completions",
                self.config.gateway_url.trim_end_matches('/')
            ))
            .bearer_auth(api_key)
            .json(&payload)
            .send()
            .await;
        match response {
            Ok(response) => json!({
                "attempted": true,
                "ok": response.status().is_success(),
                "status": response.status().as_u16(),
                "event_id": event_id,
            }),
            Err(error) => json!({
                "attempted": true,
                "ok": false,
                "error": error.to_string(),
                "event_id": event_id,
            }),
        }
    }

    pub async fn ingest_capture(&self, capture: CaptureIngest) -> IngestResult {
        let Some(api_key) = &self.config.gateway_api_key else {
            return IngestResult {
                attempted: false,
                ok: false,
                status: None,
                error: Some("gateway.api_key is not set in config.yaml".into()),
            };
        };
        let payload = build_collector_payload(&capture);
        match self
            .http_client
            .post(format!(
                "{}/collector/spend-logs",
                self.config.gateway_url.trim_end_matches('/')
            ))
            .bearer_auth(api_key)
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => IngestResult {
                attempted: true,
                ok: response.status().is_success(),
                status: Some(response.status().as_u16()),
                error: None,
            },
            Err(error) => IngestResult {
                attempted: true,
                ok: false,
                status: None,
                error: Some(error.to_string()),
            },
        }
    }

    async fn shadow_is_throttled(&self, host: &str) -> bool {
        let now = Instant::now();
        let mut shadows = self.last_shadow_by_host.lock().await;
        if let Some(last_shadow) = shadows.get(host) {
            if now.duration_since(*last_shadow)
                < Duration::from_secs(self.config.shadow_min_interval_seconds)
            {
                return true;
            }
        }
        shadows.insert(host.to_string(), now);
        false
    }
}

fn build_collector_payload(capture: &CaptureIngest) -> Value {
    let app = capture.attribution.destination_app.as_str();
    let status_code = capture
        .response_payload
        .get("status_code")
        .and_then(Value::as_u64);
    let status = if status_code.is_some_and(|code| code >= 400) {
        "failure"
    } else {
        "success"
    };
    json!({
            "logs": [{
                "request_id": format!("relay-{}", capture.event_id),
                "call_type": "relay_capture",
                "model": format!("{}-ai", if app.is_empty() { "local-ai" } else { app }),
                "api_base": format!("https://{}", capture.host),
                "spend": 0,
                "total_tokens": 0,
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "startTime": capture.started_at.to_rfc3339(),
                "endTime": capture.ended_at.to_rfc3339(),
                "request_duration_ms": capture.duration_ms,
                "status": status,
                "request_tags": ["litellm-relay", app],
                "metadata": {
                    "source": "litellm-relay",
                    "runtime": "rust",
                    "app": app,
                    "destination_app": app,
                    "attribution_source": capture.attribution.attribution_source,
                    "attribution_confidence": capture.attribution.attribution_confidence,
                    "process_lookup_status": capture.attribution.process_lookup_status,
                    "process_identity": capture.attribution.process_identity.as_deref(),
                    "traffic_kind": capture.classification.kind,
                    "traffic_reason": capture.classification.reason,
                    "host": capture.host,
                    "method": capture.method,
                    "path": capture.path,
                    "status_code": status_code,
                    "device_id": hostname(),
                    "local_user": std::env::var("USER").unwrap_or_default(),
                    "relay_event_id": capture.event_id,
                },
                "proxy_server_request": capture.request_payload,
                "response": capture.response_payload,
            }]
    })
}

#[derive(Debug)]
pub struct CaptureIngest {
    pub event_id: String,
    pub host: String,
    pub attribution: AppAttribution,
    pub method: String,
    pub path: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub request_payload: Value,
    pub response_payload: Value,
    pub duration_ms: u64,
    pub classification: TrafficClassification,
}

#[derive(Debug, Serialize)]
pub struct IngestResult {
    pub attempted: bool,
    pub ok: bool,
    pub status: Option<u16>,
    pub error: Option<String>,
}

fn build_shadow_payload(event: &Value, config: &RelayConfig, event_id: &str) -> Value {
    let host = event
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut hasher = Sha256::new();
    hasher.update(host.as_bytes());
    let host_hash = format!("{:x}", hasher.finalize());
    let app = event.get("app").and_then(Value::as_str).unwrap_or("ai");
    let method = event
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("CONNECT");
    json!({
        "model": config.shadow_model,
        "messages": [
            {
                "role": "system",
                "content": "You confirm receipt of redacted LiteLLM Relay shadow events.",
            },
            {
                "role": "user",
                "content": format!(
                    "Return exactly OK. source={app} method={method} event_id={event_id} host_hash={}",
                    &host_hash[..16]
                ),
            },
        ],
        "metadata": {
            "source": "litellm-relay",
            "runtime": "rust",
            "shadow_source": app,
            "event_id": event_id,
            "host_hash": host_hash,
            "method": method,
            "timestamp": Utc::now().to_rfc3339(),
        },
    })
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use serde_json::json;

    use super::*;
    use crate::{
        apps::classify_app_attribution,
        traffic::{TrafficClassification, TrafficKind},
    };

    #[test]
    fn should_include_destination_and_process_attribution_in_collector_metadata() {
        let timestamp = DateTime::parse_from_rfc3339("2026-07-13T00:00:00Z")
            .expect("timestamp should parse")
            .with_timezone(&Utc);
        let capture = CaptureIngest {
            event_id: "event-1".into(),
            host: "api.openai.com".into(),
            attribution: classify_app_attribution("api.openai.com", &[]),
            method: "POST".into(),
            path: "/v1/responses".into(),
            started_at: timestamp,
            ended_at: timestamp,
            request_payload: json!({"body_preview": "{\"model\":\"gpt-5\"}"}),
            response_payload: json!({"status_code": 200}),
            duration_ms: 25,
            classification: TrafficClassification {
                kind: TrafficKind::AiRequest,
                reason: "openai_api_path",
            },
        };

        let payload = build_collector_payload(&capture);
        let metadata = &payload["logs"][0]["metadata"];

        assert_eq!(metadata["app"], "codex");
        assert_eq!(metadata["destination_app"], "codex");
        assert_eq!(metadata["attribution_source"], "known_app_catalog");
        assert_eq!(metadata["attribution_confidence"], "high");
        assert_eq!(metadata["process_lookup_status"], "not_attempted");
        assert!(metadata["process_identity"].is_null());
        assert_eq!(metadata["traffic_reason"], "openai_api_path");
    }
}
