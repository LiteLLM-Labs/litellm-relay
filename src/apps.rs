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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_classify_known_hosts() {
        assert_eq!(classify_known_app("www.notion.so"), Some("notion"));
        assert_eq!(classify_known_app("chat.openai.com"), Some("codex"));
        assert_eq!(classify_known_app("example.com"), None);
    }
}
