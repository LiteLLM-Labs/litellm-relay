use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::{
    apps::AppAttribution,
    config::RelayConfig,
    credential::{check_client, check_credential, expiry_state, CredentialCheck, ExpiryState},
    system::hostname,
    traffic::TrafficClassification,
};

const CREDENTIAL_CHECK_TTL: Duration = Duration::from_secs(60);

pub struct GatewayClient {
    config: Arc<RelayConfig>,
    http_client: reqwest::Client,
    last_shadow_by_host: Mutex<HashMap<String, Instant>>,
    credential_cache: Arc<CredentialCache>,
}

#[derive(Clone)]
struct CachedCredentialCheck {
    cached_at: Instant,
    checked_at: DateTime<Utc>,
    gateway_url: String,
    api_key: String,
    check: CredentialCheck,
}

/// The live credential check behind `/api/status`. An entry only speaks for
/// the credential pair it probed: a re-enrolled key misses the cache and is
/// checked synchronously instead of inheriting the old verdict. A fresh entry
/// is served as is, a stale one is served immediately while a single
/// background probe replaces it, so only the very first call for a pair waits
/// on the Gateway.
struct CredentialCache {
    ttl: Duration,
    http: reqwest::Client,
    state: StdMutex<CredentialCacheState>,
}

#[derive(Default)]
struct CredentialCacheState {
    entry: Option<CachedCredentialCheck>,
    refreshing: bool,
}

impl CredentialCache {
    fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            http: check_client(),
            state: StdMutex::new(CredentialCacheState::default()),
        }
    }

    async fn checked(self: &Arc<Self>, gateway_url: &str, api_key: &str) -> CachedCredentialCheck {
        let entry = self
            .lock_state()
            .entry
            .clone()
            .filter(|entry| entry.gateway_url == gateway_url && entry.api_key == api_key);
        match entry {
            Some(entry) if entry.cached_at.elapsed() < self.ttl => entry,
            Some(stale) => {
                self.refresh_in_background(gateway_url, api_key);
                stale
            }
            None => {
                let fresh = self.probe(gateway_url, api_key).await;
                self.lock_state().entry = Some(fresh.clone());
                fresh
            }
        }
    }

    async fn probe(&self, gateway_url: &str, api_key: &str) -> CachedCredentialCheck {
        let check = check_credential(&self.http, gateway_url, api_key).await;
        CachedCredentialCheck {
            cached_at: Instant::now(),
            checked_at: Utc::now(),
            gateway_url: gateway_url.to_string(),
            api_key: api_key.to_string(),
            check,
        }
    }

    /// A background probe that lands after a re-enroll must not evict the new
    /// credential's entry: only store when the entry still speaks for the pair
    /// this probe checked.
    fn store_background(&self, fresh: CachedCredentialCheck) {
        let mut state = self.lock_state();
        let same_pair = state.entry.as_ref().is_none_or(|entry| {
            entry.gateway_url == fresh.gateway_url && entry.api_key == fresh.api_key
        });
        if same_pair {
            state.entry = Some(fresh);
        }
        state.refreshing = false;
    }

    fn refresh_in_background(self: &Arc<Self>, gateway_url: &str, api_key: &str) {
        {
            let mut state = self.lock_state();
            if state.refreshing {
                return;
            }
            state.refreshing = true;
        }
        let cache = Arc::clone(self);
        let gateway_url = gateway_url.to_string();
        let api_key = api_key.to_string();
        tokio::spawn(async move {
            let fresh = cache.probe(&gateway_url, &api_key).await;
            cache.store_background(fresh);
        });
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, CredentialCacheState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
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
            credential_cache: Arc::new(CredentialCache::new(CREDENTIAL_CHECK_TTL)),
        }
    }

    /// Stored credential state for `/api/status`; the live check comes from
    /// `CredentialCache`. The credential currently saved in `config.yaml` is
    /// probed, so re-running `relay setup` shows up here without a restart.
    pub async fn credential_status(&self) -> CredentialStatus {
        let (gateway_url, api_key, enrolled_at, expires_at) = match crate::config::load_settings() {
            Ok(settings) => (
                settings.gateway.url,
                settings.gateway.api_key,
                settings.gateway.enrolled_at,
                settings.gateway.expires_at,
            ),
            Err(_) => (
                self.config.gateway_url.clone(),
                self.config.gateway_api_key.clone(),
                self.config.gateway_enrolled_at,
                self.config.gateway_expires_at,
            ),
        };
        let expiry = expiry_state(expires_at, Utc::now());
        let Some(api_key) = api_key else {
            return CredentialStatus {
                configured: false,
                check: None,
                checked_at: None,
                enrolled_at,
                expires_at,
                expiry,
            };
        };
        let checked = self.credential_cache.checked(&gateway_url, &api_key).await;
        CredentialStatus {
            configured: true,
            check: Some(checked.check),
            checked_at: Some(checked.checked_at),
            enrolled_at,
            expires_at,
            expiry,
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

#[derive(Clone, Debug, PartialEq)]
pub struct CredentialStatus {
    pub configured: bool,
    pub check: Option<CredentialCheck>,
    pub checked_at: Option<DateTime<Utc>>,
    pub enrolled_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub expiry: ExpiryState,
}

impl CredentialStatus {
    pub fn to_json(&self) -> Value {
        let (state, detail) = match &self.check {
            None => ("missing", None),
            Some(CredentialCheck::Valid) => ("valid", None),
            Some(CredentialCheck::Restricted { detail }) => ("restricted", Some(detail.as_str())),
            Some(CredentialCheck::Rejected { detail }) => ("rejected", Some(detail.as_str())),
            Some(CredentialCheck::Unverifiable { detail }) => {
                ("unverifiable", Some(detail.as_str()))
            }
        };
        let expiry = match self.expiry {
            ExpiryState::Unknown => "unknown",
            ExpiryState::Ok { .. } => "ok",
            ExpiryState::ExpiringSoon { .. } => "expiring_soon",
            ExpiryState::Expired { .. } => "expired",
        };
        json!({
            "configured": self.configured,
            "state": state,
            "detail": detail,
            "checked_at": self.checked_at.map(|at| at.to_rfc3339()),
            "enrolled_at": self.enrolled_at.map(|at| at.to_rfc3339()),
            "expires_at": self.expires_at.map(|at| at.to_rfc3339()),
            "expiry": expiry,
        })
    }
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

    fn at(rfc3339: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(rfc3339)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn should_report_missing_credential() {
        let status = CredentialStatus {
            configured: false,
            check: None,
            checked_at: None,
            enrolled_at: None,
            expires_at: None,
            expiry: ExpiryState::Unknown,
        };

        assert_eq!(
            status.to_json(),
            json!({
                "configured": false,
                "state": "missing",
                "detail": null,
                "checked_at": null,
                "enrolled_at": null,
                "expires_at": null,
                "expiry": "unknown",
            })
        );
    }

    #[test]
    fn should_report_valid_credential_with_timestamps() {
        let expires_at = at("2026-09-22T21:27:58.096Z");
        let status = CredentialStatus {
            configured: true,
            check: Some(CredentialCheck::Valid),
            checked_at: Some(at("2026-09-21T22:00:00Z")),
            enrolled_at: Some(at("2026-09-21T21:27:58Z")),
            expires_at: Some(expires_at),
            expiry: ExpiryState::ExpiringSoon { at: expires_at },
        };

        assert_eq!(
            status.to_json(),
            json!({
                "configured": true,
                "state": "valid",
                "detail": null,
                "checked_at": "2026-09-21T22:00:00+00:00",
                "enrolled_at": "2026-09-21T21:27:58+00:00",
                "expires_at": "2026-09-22T21:27:58.096+00:00",
                "expiry": "expiring_soon",
            })
        );
    }

    #[test]
    fn should_report_rejected_credential_with_gateway_detail() {
        let status = CredentialStatus {
            configured: true,
            check: Some(CredentialCheck::Rejected {
                detail: "Authentication Error - Expired Key".into(),
            }),
            checked_at: Some(at("2026-09-21T22:28:15Z")),
            enrolled_at: None,
            expires_at: None,
            expiry: ExpiryState::Unknown,
        };

        let payload = status.to_json();
        assert_eq!(payload["state"], "rejected");
        assert_eq!(payload["detail"], "Authentication Error - Expired Key");
        assert_eq!(payload["expiry"], "unknown");
    }

    #[test]
    fn should_report_unverifiable_credential_and_expired_state() {
        let expires_at = at("2026-09-21T22:27:58Z");
        let status = CredentialStatus {
            configured: true,
            check: Some(CredentialCheck::Unverifiable {
                detail: "HTTP 502".into(),
            }),
            checked_at: Some(at("2026-09-21T23:00:00Z")),
            enrolled_at: None,
            expires_at: Some(expires_at),
            expiry: ExpiryState::Expired { at: expires_at },
        };

        let payload = status.to_json();
        assert_eq!(payload["state"], "unverifiable");
        assert_eq!(payload["detail"], "HTTP 502");
        assert_eq!(payload["expiry"], "expired");
        assert_eq!(payload["expires_at"], "2026-09-21T22:27:58+00:00");
    }

    #[test]
    fn should_report_ok_expiry() {
        let expires_at = at("2026-09-30T00:00:00Z");
        let status = CredentialStatus {
            configured: true,
            check: Some(CredentialCheck::Valid),
            checked_at: None,
            enrolled_at: None,
            expires_at: Some(expires_at),
            expiry: ExpiryState::Ok { at: expires_at },
        };

        assert_eq!(status.to_json()["expiry"], "ok");
    }

    #[test]
    fn should_report_restricted_credential_with_gateway_detail() {
        let status = CredentialStatus {
            configured: true,
            check: Some(CredentialCheck::Restricted {
                detail: "Virtual key is not allowed to call this route".into(),
            }),
            checked_at: Some(at("2026-09-21T23:00:00Z")),
            enrolled_at: None,
            expires_at: None,
            expiry: ExpiryState::Unknown,
        };

        let payload = status.to_json();
        assert_eq!(payload["state"], "restricted");
        assert_eq!(
            payload["detail"],
            "Virtual key is not allowed to call this route"
        );
    }

    #[tokio::test]
    async fn should_probe_a_re_enrolled_key_instead_of_serving_the_old_verdict() {
        use crate::credential::test_support::serve_sequence;
        use std::time::Duration;

        let gateway = serve_sequence(&[
            (
                "401 Unauthorized",
                r#"{"error":{"message":"expired","type":"expired_key","code":"401"}}"#,
            ),
            ("200 OK", r#"{"data":[]}"#),
        ])
        .await;
        let cache = Arc::new(CredentialCache::new(Duration::from_secs(3600)));

        assert!(matches!(
            cache.checked(&gateway, "sk-old").await.check,
            CredentialCheck::Rejected { .. }
        ));

        let checked = cache.checked(&gateway, "sk-new").await;
        assert_eq!(
            checked.check,
            CredentialCheck::Valid,
            "a re-enrolled key must be probed immediately, not serve the cached rejection"
        );
        assert_eq!(checked.api_key, "sk-new");
    }

    #[tokio::test]
    async fn should_not_let_a_late_background_probe_evict_the_re_enrolled_entry() {
        use crate::credential::test_support::serve_once;
        use std::time::Duration;

        let gateway = serve_once("200 OK", r#"{"data":[]}"#).await;
        let cache = Arc::new(CredentialCache::new(Duration::from_secs(3600)));
        assert_eq!(
            cache.checked(&gateway, "sk-new").await.check,
            CredentialCheck::Valid
        );

        let stale_probe = CachedCredentialCheck {
            cached_at: Instant::now(),
            checked_at: Utc::now(),
            gateway_url: gateway.clone(),
            api_key: "sk-old".to_string(),
            check: CredentialCheck::Rejected {
                detail: "expired".to_string(),
            },
        };
        cache.store_background(stale_probe);
        let entry = cache.lock_state().entry.clone().unwrap();
        assert_eq!(entry.api_key, "sk-new");
        assert_eq!(entry.check, CredentialCheck::Valid);

        let fresh_probe = CachedCredentialCheck {
            cached_at: Instant::now(),
            checked_at: Utc::now(),
            gateway_url: gateway.clone(),
            api_key: "sk-new".to_string(),
            check: CredentialCheck::Rejected {
                detail: "expired".to_string(),
            },
        };
        cache.store_background(fresh_probe);
        let entry = cache.lock_state().entry.clone().unwrap();
        assert_eq!(entry.api_key, "sk-new");
        assert!(matches!(entry.check, CredentialCheck::Rejected { .. }));
    }

    #[tokio::test]
    async fn should_serve_the_stale_check_while_the_gateway_hangs_and_refresh_behind_it() {
        use crate::credential::test_support::serve_once_then_hung;
        use std::time::Duration;

        let cache = Arc::new(CredentialCache::new(Duration::ZERO));
        let (release, released) = tokio::sync::oneshot::channel();
        let gateway = serve_once_then_hung("200 OK", r#"{"data":[]}"#, released).await;
        assert_eq!(
            cache.checked(&gateway, "sk-live").await.check,
            CredentialCheck::Valid
        );

        let started = Instant::now();
        let served = cache.checked(&gateway, "sk-live").await;
        assert_eq!(served.check, CredentialCheck::Valid);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "a stale entry must be served without waiting on the Gateway, took {:?}",
            started.elapsed()
        );
        assert!(
            cache.lock_state().refreshing,
            "the stale read must start exactly one background refresh"
        );

        release.send(()).unwrap();
        let refreshed = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let entry = cache.checked(&gateway, "sk-live").await;
                if entry.check != CredentialCheck::Valid {
                    return entry;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the background refresh must land once the Gateway answers");
        assert!(
            matches!(refreshed.check, CredentialCheck::Unverifiable { .. }),
            "{:?}",
            refreshed.check
        );
    }
}
