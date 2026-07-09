use std::{
    fs,
    io::{self, Write},
    process::Command,
};

use anyhow::{anyhow, Result};

use crate::system::home_dir;

pub fn run_setup(gateway_url: Option<String>, api_key: Option<String>) -> Result<()> {
    let gateway_url =
        gateway_url.unwrap_or_else(|| prompt("LiteLLM Gateway URL", "http://127.0.0.1:4000"));
    let login_url = format!(
        "{}/ui/login?redirect_to=/ui/?login=success&page=api-keys",
        gateway_url.trim_end_matches('/')
    );
    println!("Opening LiteLLM Gateway login/API key page:");
    println!("{login_url}");
    let _ = Command::new("open").arg(&login_url).status();

    let api_key = api_key.unwrap_or_else(|| prompt("Paste Relay Gateway key", ""));
    if api_key.trim().is_empty() {
        return Err(anyhow!("setup requires a LiteLLM Gateway API key"));
    }

    let relay_home = home_dir().join(".litellm-relay");
    fs::create_dir_all(&relay_home)?;
    let env_path = relay_home.join("env");
    let contents = format!(
        "LITELLM_RELAY_HOST=127.0.0.1\n\
         LITELLM_RELAY_PORT=4142\n\
         LITELLM_RELAY_LOG_PATH={}/relay.log.jsonl\n\
         LITELLM_GATEWAY_URL={}\n\
         LITELLM_GATEWAY_API_KEY={}\n\
         LITELLM_RELAY_SHADOW_ENABLED=0\n\
         LITELLM_RELAY_CAPTURE_PAYLOADS=1\n\
         LITELLM_RELAY_MITM_CA_DIR={}/mitm\n",
        relay_home.display(),
        gateway_url.trim_end_matches('/'),
        api_key.trim(),
        relay_home.display()
    );
    fs::write(&env_path, contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&env_path, fs::Permissions::from_mode(0o600))?;
    }
    println!("Wrote {}", env_path.display());
    Ok(())
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
