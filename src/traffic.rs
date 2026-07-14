use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrafficKind {
    AiRequest,
    Telemetry,
    AppTraffic,
    StaticAsset,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TrafficClassification {
    pub kind: TrafficKind,
    pub reason: &'static str,
}

impl TrafficClassification {
    pub fn ai(reason: &'static str) -> Self {
        Self {
            kind: TrafficKind::AiRequest,
            reason,
        }
    }

    pub fn telemetry(reason: &'static str) -> Self {
        Self {
            kind: TrafficKind::Telemetry,
            reason,
        }
    }

    pub fn app_traffic(reason: &'static str) -> Self {
        Self {
            kind: TrafficKind::AppTraffic,
            reason,
        }
    }

    pub fn static_asset(reason: &'static str) -> Self {
        Self {
            kind: TrafficKind::StaticAsset,
            reason,
        }
    }

    pub fn is_ai_request(&self) -> bool {
        self.kind == TrafficKind::AiRequest
    }
}

pub struct CapturedTraffic<'a> {
    pub app: &'a str,
    pub host: &'a str,
    pub method: &'a str,
    pub path: &'a str,
    pub request_headers: &'a HashMap<String, String>,
    pub request_payload: &'a Value,
    pub response_payload: &'a Value,
}

pub fn classify_captured_traffic(capture: CapturedTraffic<'_>) -> TrafficClassification {
    let host = capture.host.to_ascii_lowercase();
    let path = capture.path.to_ascii_lowercase();
    let body_preview = body_preview(capture.request_payload);
    let content_type = header(capture.request_headers, "content-type");
    let accept = header(capture.request_headers, "accept");

    if is_websocket_upgrade(capture.request_headers)
        || response_status(capture.response_payload) == Some(101)
    {
        return TrafficClassification::telemetry("websocket_upgrade");
    }

    if is_static_asset_path(&path) || is_static_asset_accept(accept) {
        return TrafficClassification::static_asset("static_asset");
    }

    if is_known_telemetry_path(&host, &path) {
        return TrafficClassification::telemetry("known_telemetry_path");
    }

    if capture.app == "notion" && is_notion_realtime_noise(&path, &body_preview) {
        return TrafficClassification::telemetry("notion_realtime");
    }

    if capture.app == "notion" && is_notion_ai_request(&path, &body_preview) {
        return TrafficClassification::ai("notion_ai_marker");
    }

    if is_openai_api_host(&host) && is_openai_ai_path(&path) {
        return TrafficClassification::ai("openai_api_path");
    }

    if capture.app == "codex" && is_codex_ai_path(&path) {
        return TrafficClassification::ai("codex_ai_path");
    }

    if capture.method.eq_ignore_ascii_case("POST")
        && content_type.is_some_and(|value| value.contains("application/json"))
        && contains_ai_marker(&path, &body_preview)
    {
        return TrafficClassification::ai("json_ai_marker");
    }

    TrafficClassification::app_traffic("unclassified_app_traffic")
}

fn header<'a>(headers: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    headers.get(key).map(String::as_str)
}

fn body_preview(payload: &Value) -> String {
    payload
        .get("body")
        .or_else(|| payload.get("body_preview"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn response_status(payload: &Value) -> Option<u16> {
    payload
        .get("status_code")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
}

fn is_websocket_upgrade(headers: &HashMap<String, String>) -> bool {
    let connection = header(headers, "connection").unwrap_or_default();
    let upgrade = header(headers, "upgrade").unwrap_or_default();
    connection.to_ascii_lowercase().contains("upgrade") || upgrade.eq_ignore_ascii_case("websocket")
}

fn is_static_asset_path(path: &str) -> bool {
    let path_without_query = path.split('?').next().unwrap_or(path);
    path_without_query == "/sw.js"
        || path_without_query.ends_with(".js")
        || path_without_query.ends_with(".css")
        || path_without_query.ends_with(".png")
        || path_without_query.ends_with(".jpg")
        || path_without_query.ends_with(".jpeg")
        || path_without_query.ends_with(".gif")
        || path_without_query.ends_with(".svg")
        || path_without_query.ends_with(".webp")
        || path_without_query.ends_with(".ico")
        || path_without_query.ends_with(".woff")
        || path_without_query.ends_with(".woff2")
}

fn is_static_asset_accept(accept: Option<&str>) -> bool {
    accept.is_some_and(|value| {
        let value = value.to_ascii_lowercase();
        value.starts_with("image/") || value.contains("text/css")
    })
}

fn is_known_telemetry_path(host: &str, path: &str) -> bool {
    host == "ab.chatgpt.com"
        || path.starts_with("/ces/")
        || path.contains("/telemetry/")
        || path.contains("/beacons/")
        || path.contains("/rgstr")
        || path.contains("/v1/initialize")
        || path.contains("sentry")
        || path.contains("statsig")
}

fn is_notion_realtime_noise(path: &str, body_preview: &str) -> bool {
    if !path.starts_with("/primus-v8/") {
        return false;
    }
    body_preview.is_empty()
        || body_preview == "1"
        || body_preview == "2"
        || body_preview == "3"
        || body_preview.starts_with("4\"primus::pong::")
        || body_preview.contains("\"/api/v1/registerbatchsubscriptions\"")
}

fn is_codex_ai_path(path: &str) -> bool {
    path.starts_with("/v1/chat/completions")
        || path.starts_with("/v1/responses")
        || path.starts_with("/v1/messages")
        || path.starts_with("/backend-api/conversation")
        || path.starts_with("/backend-api/codex")
        || path.starts_with("/backend-api/responses")
}

fn is_notion_ai_request(path: &str, body_preview: &str) -> bool {
    if path.contains("getaiusageeligibility") {
        return false;
    }
    contains_ai_marker(path, body_preview)
}

fn is_openai_api_host(host: &str) -> bool {
    host == "api.openai.com" || host.ends_with(".api.openai.com")
}

fn is_openai_ai_path(path: &str) -> bool {
    path.starts_with("/v1/chat/completions")
        || path.starts_with("/v1/responses")
        || path.starts_with("/v1/messages")
        || path.starts_with("/v1/completions")
}

fn contains_ai_marker(path: &str, body_preview: &str) -> bool {
    const MARKERS: [&str; 11] = [
        "ai",
        "assistant",
        "completion",
        "generate",
        "inference",
        "llm",
        "prompt",
        "summarize",
        "translate",
        "writer",
        "q&a",
    ];
    MARKERS
        .iter()
        .any(|marker| path.contains(marker) || body_preview.contains(marker))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn headers(values: &[(&str, &str)]) -> HashMap<String, String> {
        values
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect()
    }

    fn capture<'a>(
        app: &'a str,
        host: &'a str,
        method: &'a str,
        path: &'a str,
        request_headers: &'a HashMap<String, String>,
        request_payload: &'a Value,
        response_payload: &'a Value,
    ) -> CapturedTraffic<'a> {
        CapturedTraffic {
            app,
            host,
            method,
            path,
            request_headers,
            request_payload,
            response_payload,
        }
    }

    #[test]
    fn classifies_notion_primus_pong_as_telemetry() {
        let request_headers = headers(&[("content-type", "text/plain;charset=UTF-8")]);
        let request_payload = json!({"body": "4\"primus::pong::1783641305148\""});
        let response_payload = json!({"status_code": 200});

        let classification = classify_captured_traffic(capture(
            "notion",
            "msgstore-001.www.notion.so",
            "POST",
            "/primus-v8/?transport=polling",
            &request_headers,
            &request_payload,
            &response_payload,
        ));

        assert_eq!(classification.kind, TrafficKind::Telemetry);
        assert_eq!(classification.reason, "notion_realtime");
    }

    #[test]
    fn classifies_codex_telemetry_as_telemetry() {
        let request_headers = headers(&[("content-type", "text/plain")]);
        let request_payload = json!({"body": "{\"message\":\"Item not found\"}"});
        let response_payload = json!({"status_code": 202});

        let classification = classify_captured_traffic(capture(
            "codex",
            "chat.openai.com",
            "POST",
            "/ces/v1/telemetry/intake?ddforward=/api/v2/logs",
            &request_headers,
            &request_payload,
            &response_payload,
        ));

        assert_eq!(classification.kind, TrafficKind::Telemetry);
        assert_eq!(classification.reason, "known_telemetry_path");
    }

    #[test]
    fn classifies_openai_responses_api_as_ai_request() {
        let request_headers = headers(&[("content-type", "application/json")]);
        let request_payload = json!({"body": "{\"model\":\"gpt-5\",\"input\":\"hi\"}"});
        let response_payload = json!({"status_code": 200});

        let classification = classify_captured_traffic(capture(
            "codex",
            "api.openai.com",
            "POST",
            "/v1/responses",
            &request_headers,
            &request_payload,
            &response_payload,
        ));

        assert!(classification.is_ai_request());
        assert_eq!(classification.reason, "openai_api_path");
    }

    #[test]
    fn classifies_notion_ai_marker_as_ai_request() {
        let request_headers = headers(&[("content-type", "application/json")]);
        let request_payload = json!({"body": "{\"prompt\":\"summarize this page\"}"});
        let response_payload = json!({"status_code": 200});

        let classification = classify_captured_traffic(capture(
            "notion",
            "www.notion.so",
            "POST",
            "/api/v3/runInference",
            &request_headers,
            &request_payload,
            &response_payload,
        ));

        assert!(classification.is_ai_request());
        assert_eq!(classification.reason, "notion_ai_marker");
    }

    #[test]
    fn classifies_notion_billing_as_app_traffic() {
        let request_headers = headers(&[("content-type", "application/json")]);
        let request_payload = json!({"body": "{\"spaceId\":\"space\"}"});
        let response_payload = json!({"status_code": 200});

        let classification = classify_captured_traffic(capture(
            "notion",
            "www.notion.so",
            "POST",
            "/api/v3/getBillingData",
            &request_headers,
            &request_payload,
            &response_payload,
        ));

        assert_eq!(classification.kind, TrafficKind::AppTraffic);
    }
}
