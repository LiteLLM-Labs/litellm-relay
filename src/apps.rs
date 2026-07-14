use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use crate::config::normalize_host;

const KNOWN_APPS_JSON: &str = include_str!("static/known_apps.json");

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KnownApp {
    pub id: String,
    pub label: String,
    pub description: String,
    pub logo: AppLogo,
    pub matchers: AppMatchers,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppLogo {
    pub text: String,
    pub background: String,
    pub foreground: String,
    pub border: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AppMatchers {
    pub domains: Vec<String>,
    pub refs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AppAttribution {
    pub destination_app: String,
    pub attribution_source: AttributionSource,
    pub attribution_confidence: AttributionConfidence,
    pub process_lookup_status: ProcessLookupStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_identity: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionSource {
    KnownAppCatalog,
    ConfiguredAiDomain,
    Unmatched,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionConfidence {
    High,
    Medium,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessLookupStatus {
    NotAttempted,
}

static KNOWN_APPS: OnceLock<Vec<KnownApp>> = OnceLock::new();

pub fn known_apps() -> &'static [KnownApp] {
    KNOWN_APPS
        .get_or_init(|| {
            serde_json::from_str(KNOWN_APPS_JSON).expect("known app catalog must be valid JSON")
        })
        .as_slice()
}

pub fn default_ai_domains() -> Vec<String> {
    known_apps()
        .iter()
        .flat_map(|app| app.matchers.domains.iter().cloned())
        .collect()
}

pub fn default_notion_domains() -> Vec<String> {
    known_apps()
        .iter()
        .find(|app| app.id == "notion")
        .map(|app| app.matchers.domains.clone())
        .unwrap_or_default()
}

pub fn classify_known_app(host: &str) -> Option<&'static str> {
    let normalized = normalize_host(host);
    known_apps()
        .iter()
        .find(|app| {
            app.matchers
                .domains
                .iter()
                .any(|domain| normalized == *domain || normalized.ends_with(&format!(".{domain}")))
        })
        .map(|app| app.id.as_str())
}

pub fn classify_app_attribution(host: &str, ai_domains: &[String]) -> AppAttribution {
    let normalized = normalize_host(host);
    let (destination_app, attribution_source, attribution_confidence) =
        if let Some(app_id) = classify_known_app(&normalized) {
            (
                app_id.to_string(),
                AttributionSource::KnownAppCatalog,
                AttributionConfidence::High,
            )
        } else if domain_matches_host(&normalized, ai_domains) {
            (
                "ai".to_string(),
                AttributionSource::ConfiguredAiDomain,
                AttributionConfidence::Medium,
            )
        } else {
            (
                "unknown".to_string(),
                AttributionSource::Unmatched,
                AttributionConfidence::None,
            )
        };

    AppAttribution {
        destination_app,
        attribution_source,
        attribution_confidence,
        process_lookup_status: ProcessLookupStatus::NotAttempted,
        process_identity: None,
    }
}

pub fn domain_matches_host(host: &str, domains: &[String]) -> bool {
    let normalized = normalize_host(host);
    domains
        .iter()
        .any(|domain| normalized == *domain || normalized.ends_with(&format!(".{domain}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_classify_known_hosts() {
        assert_eq!(classify_known_app("www.notion.so"), Some("notion"));
        assert_eq!(classify_known_app("chat.openai.com"), Some("codex"));
        assert_eq!(classify_known_app("example.com"), None);
    }

    #[test]
    fn should_attribute_known_destination_without_process_identity() {
        let attribution = classify_app_attribution("api.openai.com", &[]);

        assert_eq!(attribution.destination_app, "codex");
        assert_eq!(
            attribution.attribution_source,
            AttributionSource::KnownAppCatalog
        );
        assert_eq!(
            attribution.attribution_confidence,
            AttributionConfidence::High
        );
        assert_eq!(
            attribution.process_lookup_status,
            ProcessLookupStatus::NotAttempted
        );
        assert_eq!(attribution.process_identity, None);
    }

    #[test]
    fn should_attribute_configured_ai_destination_without_process_identity() {
        let ai_domains = vec!["example.ai".to_string()];
        let attribution = classify_app_attribution("api.example.ai", &ai_domains);

        assert_eq!(attribution.destination_app, "ai");
        assert_eq!(
            attribution.attribution_source,
            AttributionSource::ConfiguredAiDomain
        );
        assert_eq!(
            attribution.attribution_confidence,
            AttributionConfidence::Medium
        );
        assert_eq!(
            attribution.process_lookup_status,
            ProcessLookupStatus::NotAttempted
        );
        assert_eq!(attribution.process_identity, None);
    }

    #[test]
    fn should_report_unknown_destination_and_unknown_process_status() {
        let attribution = classify_app_attribution("example.com", &[]);

        assert_eq!(attribution.destination_app, "unknown");
        assert_eq!(attribution.attribution_source, AttributionSource::Unmatched);
        assert_eq!(
            attribution.attribution_confidence,
            AttributionConfidence::None
        );
        assert_eq!(
            attribution.process_lookup_status,
            ProcessLookupStatus::NotAttempted
        );
        assert_eq!(attribution.process_identity, None);
    }
}
