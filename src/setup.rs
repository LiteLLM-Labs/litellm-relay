use std::io::{self, Write};

use anyhow::{anyhow, Result};

use crate::{
    ai_tools::{autoconfigure, AutoConfigureParams},
    auth::GatewaySsoClient,
    config::{load_settings, save_settings},
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
