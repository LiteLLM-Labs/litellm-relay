use std::io::{self, Write};

use anyhow::{anyhow, Result};

use crate::{
    ai_tools::{autoconfigure, AutoConfigureParams},
    auth::GatewaySsoClient,
    config::{load_settings, save_settings},
    credential::{check_client, check_credential, CredentialCheck},
    terminal::{print_setup_complete, print_setup_intro, print_step},
};

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
    let (api_key, user_id, team_id, expires_at) = match api_key {
        Some(api_key) => {
            println!("  Using API key from command line.");
            (api_key, None, None, None)
        }
        None if prompt_browser_sso() => {
            let auth = GatewaySsoClient::new().login(&gateway_url).await?;
            (auth.api_key, auth.user_id, auth.team_id, auth.expires_at)
        }
        None => (prompt("Gateway API key", ""), None, None, None),
    };

    if api_key.trim().is_empty() {
        return Err(anyhow!("setup requires a LiteLLM Gateway API key"));
    }

    let check = check_credential(&check_client(), &gateway_url, api_key.trim()).await;
    if let Some(notice) = credential_notice(&check, &gateway_url)? {
        eprintln!("  {notice}");
    }

    println!();
    print_step(3, 4, "Save local Relay config");
    settings.move_gateway_to(&gateway_url);
    settings
        .gateway
        .enroll(api_key.trim().to_string(), expires_at);
    let config_path = save_settings(&settings)?;
    print_setup_complete(
        &config_path,
        user_id.as_deref(),
        team_id.as_deref(),
        expires_at,
    );

    println!();
    print_step(4, 4, "Configure installed AI tools");
    // Detect and wire up every AI tool on this device. `autoconfigure` reads
    // the config just saved and prefers the IdP, falling back to the Gateway
    // key on non-SSO setups.
    if let Err(error) = autoconfigure(AutoConfigureParams::default(), &[]).await {
        eprintln!("  Skipping AI tool auto-configuration: {error:#}");
    }
    Ok(())
}

/// Decides whether setup may save the credential it was given. A key the
/// Gateway rejects is an error, so `relay setup` exits non-zero instead of
/// printing "Setup complete" over a config no tool can use. A key the Gateway
/// cannot vouch for right now (unreachable, 5xx) is saved with a notice, since
/// a managed install may run before the network is up; the daemon re-checks it
/// and reports the result on /api/status.
fn credential_notice(check: &CredentialCheck, gateway_url: &str) -> Result<Option<String>> {
    let gateway = gateway_url.trim_end_matches('/');
    match check {
        CredentialCheck::Valid => Ok(None),
        CredentialCheck::Restricted { detail } => Ok(Some(format!(
            "Note: {gateway} accepts this key but does not let it list models ({detail}). \
             Relay saved it; auto-configure verifies it again before each run."
        ))),
        CredentialCheck::Unverifiable { detail } => Ok(Some(format!(
            "Warning: could not verify the key with {gateway} ({detail}). Relay saved it \
             and will verify it when the Gateway is reachable; see /api/status."
        ))),
        CredentialCheck::Rejected { detail } => Err(anyhow!(
            "{gateway} rejected this API key ({detail}). Nothing was saved; check the key \
             and run `relay setup` again."
        )),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const GATEWAY: &str = "https://gateway.example.com/";

    #[test]
    fn should_save_silently_when_the_gateway_accepts_the_key() {
        let notice = credential_notice(&CredentialCheck::Valid, GATEWAY).unwrap();
        assert_eq!(notice, None);
    }

    #[test]
    fn should_refuse_to_save_a_key_the_gateway_rejects() {
        let error = credential_notice(
            &CredentialCheck::Rejected {
                detail: "Authentication Error, Invalid proxy server token passed".into(),
            },
            GATEWAY,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("https://gateway.example.com rejected this API key"),
            "{error}"
        );
        assert!(error.contains("Invalid proxy server token"), "{error}");
        assert!(error.contains("Nothing was saved"), "{error}");
    }

    #[test]
    fn should_save_a_route_restricted_key_with_a_note() {
        let notice = credential_notice(
            &CredentialCheck::Restricted {
                detail: "key cannot call /v1/models".into(),
            },
            GATEWAY,
        )
        .unwrap()
        .expect("restricted keys are saved with a note");
        assert!(notice.starts_with("Note:"), "{notice}");
        assert!(notice.contains("key cannot call /v1/models"), "{notice}");
    }

    #[test]
    fn should_save_an_unverifiable_key_with_a_warning() {
        let notice = credential_notice(
            &CredentialCheck::Unverifiable {
                detail: "connection refused".into(),
            },
            GATEWAY,
        )
        .unwrap()
        .expect("unverifiable keys are saved with a warning");
        assert!(notice.starts_with("Warning:"), "{notice}");
        assert!(notice.contains("connection refused"), "{notice}");
        assert!(notice.contains("/api/status"), "{notice}");
    }
}
