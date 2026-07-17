use std::{
    io::{self, Write},
    time::Duration,
};

use anyhow::{anyhow, Result};
use chrono::Utc;

use crate::{
    ai_tools::{autoconfigure, AutoConfigureParams},
    auth::GatewaySsoClient,
    config::{load_settings, save_settings},
    terminal::{print_setup_complete, print_setup_intro, print_step, print_usage_access_warning},
};

/// Routes the RelayBar menu bar app reads usage from. `/key/info` is an
/// info route and `/user/daily/activity` a management route, so neither is
/// covered by a default key scoped to `llm_api_routes`.
const KEY_INFO_ROUTE: &str = "/key/info";
const DAILY_ACTIVITY_ROUTE: &str = "/user/daily/activity";
const USAGE_ACCESS_SUGGESTION: &str = "Ask your Gateway admin to allow these routes on the key, \
e.g. set allowed_routes to include \"info_routes\" and \"management_routes\" (or add the two routes explicitly).";

pub async fn run_setup(gateway_url: Option<String>, api_key: Option<String>) -> Result<()> {
    print_setup_intro();

    let mut settings = load_settings()?;
    let default_gateway_url = settings.gateway.url.clone();
    print_step(1, 4, "Choose your LiteLLM Gateway");
    let gateway_url = match gateway_url {
        Some(gateway_url) => {
            println!("  Gateway URL: {}", gateway_url.trim_end_matches('/'));
            gateway_url
        }
        None => prompt("Gateway URL", &default_gateway_url),
    };

    println!();
    print_step(2, 4, "Sign in");
    let (api_key, user_id, team_id) = match api_key {
        Some(api_key) => {
            println!("  Using API key from command line.");
            (api_key, None, None)
        }
        None if prompt_browser_sso() => {
            let auth = GatewaySsoClient::new().login(&gateway_url).await?;
            (auth.api_key, auth.user_id, auth.team_id)
        }
        None => (prompt("Gateway API key", ""), None, None),
    };

    if api_key.trim().is_empty() {
        return Err(anyhow!("setup requires a LiteLLM Gateway API key"));
    }

    println!();
    print_step(3, 4, "Save local Relay config");
    settings.gateway.url = gateway_url.trim_end_matches('/').to_string();
    settings.gateway.api_key = Some(api_key.trim().to_string());
    let config_path = save_settings(&settings)?;
    print_setup_complete(&config_path, user_id.as_deref(), team_id.as_deref());

    // Probe the routes RelayBar needs so an under-scoped key is caught here,
    // with a fix, rather than silently showing empty tabs later.
    let denied = probe_usage_access(&settings.gateway.url, settings.gateway.api_key.as_deref())
        .await
        .denied_routes();
    if !denied.is_empty() {
        print_usage_access_warning(&denied, USAGE_ACCESS_SUGGESTION);
    }

    println!();
    print_step(4, 4, "Configure installed AI tools");
    // Detect and wire up every AI tool on this device. `autoconfigure` reads
    // the config just saved and prefers the IdP, falling back to the Gateway
    // key on non-SSO setups.
    if let Err(error) = autoconfigure(AutoConfigureParams::default(), &[]) {
        eprintln!("  Skipping AI tool auto-configuration: {error:#}");
    }
    Ok(())
}

fn prompt_browser_sso() -> bool {
    loop {
        let value = prompt("Use browser SSO", "Y");
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "y" | "yes" => return true,
            "n" | "no" => return false,
            _ => println!("Enter 'Y' for browser SSO or 'n' to paste an API key."),
        }
    }
}

fn prompt(label: &str, default: &str) -> String {
    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    let value = line.trim();
    if value.is_empty() {
        default.to_string()
    } else {
        value.to_string()
    }
}

/// Access result for a single usage route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RouteAccess {
    Allowed,
    Denied,
    Unknown,
}

/// Outcome of probing the routes RelayBar reads usage from.
struct UsageAccess {
    key_info: RouteAccess,
    daily_activity: RouteAccess,
}

impl UsageAccess {
    fn unknown() -> Self {
        Self {
            key_info: RouteAccess::Unknown,
            daily_activity: RouteAccess::Unknown,
        }
    }

    /// Routes that returned an auth failure. Only `Denied` is surfaced;
    /// `Unknown` (network error or an inconclusive status) is left alone so a
    /// flaky probe never nags about a key that may well be fine.
    fn denied_routes(&self) -> Vec<&'static str> {
        [
            (KEY_INFO_ROUTE, self.key_info),
            (DAILY_ACTIVITY_ROUTE, self.daily_activity),
        ]
        .into_iter()
        .filter(|(_, access)| *access == RouteAccess::Denied)
        .map(|(route, _)| route)
        .collect()
    }
}

/// Maps an HTTP status to route access: 2xx is allowed, 401/403 is a permission
/// denial, anything else (400, 5xx, ...) is inconclusive.
fn classify_status(status: reqwest::StatusCode) -> RouteAccess {
    if status.is_success() {
        RouteAccess::Allowed
    } else if status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
    {
        RouteAccess::Denied
    } else {
        RouteAccess::Unknown
    }
}

/// GETs `/key/info` and `/user/daily/activity` with the key to see whether it
/// can read usage. Best-effort: any transport failure resolves to `Unknown`.
async fn probe_usage_access(gateway_url: &str, api_key: Option<&str>) -> UsageAccess {
    let Some(api_key) = api_key.map(str::trim).filter(|key| !key.is_empty()) else {
        return UsageAccess::unknown();
    };
    let Ok(client) = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    else {
        return UsageAccess::unknown();
    };
    let base = gateway_url.trim_end_matches('/');
    let today = Utc::now().format("%Y-%m-%d");
    UsageAccess {
        key_info: probe_route(&client, &format!("{base}{KEY_INFO_ROUTE}"), api_key).await,
        daily_activity: probe_route(
            &client,
            &format!("{base}{DAILY_ACTIVITY_ROUTE}?start_date={today}&end_date={today}"),
            api_key,
        )
        .await,
    }
}

async fn probe_route(client: &reqwest::Client, url: &str, api_key: &str) -> RouteAccess {
    match client.get(url).bearer_auth(api_key).send().await {
        Ok(response) => classify_status(response.status()),
        Err(_) => RouteAccess::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;

    #[test]
    fn should_classify_status_into_route_access() {
        assert_eq!(classify_status(StatusCode::OK), RouteAccess::Allowed);
        assert_eq!(classify_status(StatusCode::FORBIDDEN), RouteAccess::Denied);
        assert_eq!(
            classify_status(StatusCode::UNAUTHORIZED),
            RouteAccess::Denied
        );
        assert_eq!(
            classify_status(StatusCode::BAD_REQUEST),
            RouteAccess::Unknown
        );
        assert_eq!(
            classify_status(StatusCode::INTERNAL_SERVER_ERROR),
            RouteAccess::Unknown
        );
    }

    #[test]
    fn should_report_only_denied_routes() {
        let access = UsageAccess {
            key_info: RouteAccess::Allowed,
            daily_activity: RouteAccess::Denied,
        };
        assert_eq!(access.denied_routes(), vec![DAILY_ACTIVITY_ROUTE]);
    }

    #[test]
    fn should_report_both_denied_routes() {
        let access = UsageAccess {
            key_info: RouteAccess::Denied,
            daily_activity: RouteAccess::Denied,
        };
        assert_eq!(
            access.denied_routes(),
            vec![KEY_INFO_ROUTE, DAILY_ACTIVITY_ROUTE]
        );
    }

    #[test]
    fn should_not_warn_when_access_is_unknown_or_allowed() {
        assert!(UsageAccess::unknown().denied_routes().is_empty());
        let allowed = UsageAccess {
            key_info: RouteAccess::Allowed,
            daily_activity: RouteAccess::Allowed,
        };
        assert!(allowed.denied_routes().is_empty());
    }
}
