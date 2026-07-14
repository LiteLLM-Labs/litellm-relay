use std::{fs, path::PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    ai_tools::idp::{sign_in, token_expiry},
    config::relay_home,
};

const TOKEN_REFRESH_SKEW_SECONDS: i64 = 60;

#[derive(Debug, Deserialize, Serialize)]
struct CachedToken {
    token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    exp: Option<i64>,
}

/// Returns a valid IdP bearer token for any onboarded tool. Reuses the cached
/// token until it nears expiry, then runs a browser sign-in against
/// `authorize_url`. The token is identity-scoped, so it is shared across tools.
pub fn ensure_token(authorize_url: &str) -> Result<String> {
    if let Some(token) = valid_cached_token()? {
        return Ok(token);
    }
    if authorize_url.trim().is_empty() {
        bail!("no IdP configured; run `relay onboard` first");
    }
    let token = sign_in(authorize_url)?;
    cache_token(&token)?;
    Ok(token)
}

fn valid_cached_token() -> Result<Option<String>> {
    let path = token_cache_path();
    if !path.exists() {
        return Ok(None);
    }
    let contents =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let cached: CachedToken = match serde_json::from_str(&contents) {
        Ok(cached) => cached,
        Err(_) => return Ok(None),
    };
    Ok(fresh_token(cached, Utc::now().timestamp()))
}

fn fresh_token(cached: CachedToken, now: i64) -> Option<String> {
    match cached.exp {
        Some(exp) if exp <= now + TOKEN_REFRESH_SKEW_SECONDS => None,
        _ => Some(cached.token),
    }
}

fn cache_token(token: &str) -> Result<()> {
    let path = token_cache_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let cached = CachedToken {
        token: token.to_string(),
        exp: token_expiry(token),
    };
    fs::write(&path, serde_json::to_string(&cached)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    secure_file(&path)?;
    Ok(())
}

fn token_cache_path() -> PathBuf {
    relay_home().join("identity-token.json")
}

fn secure_file(path: &PathBuf) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to secure {}", path.display()))?;
    }
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_reuse_token_that_is_not_near_expiry() {
        let cached = CachedToken {
            token: "still-good".into(),
            exp: Some(1_000 + TOKEN_REFRESH_SKEW_SECONDS + 1),
        };
        assert_eq!(fresh_token(cached, 1_000), Some("still-good".into()));
    }

    #[test]
    fn should_refresh_token_within_skew_window() {
        let cached = CachedToken {
            token: "about-to-expire".into(),
            exp: Some(1_000 + TOKEN_REFRESH_SKEW_SECONDS),
        };
        assert_eq!(fresh_token(cached, 1_000), None);
    }

    #[test]
    fn should_reuse_token_without_expiry() {
        let cached = CachedToken {
            token: "no-exp".into(),
            exp: None,
        };
        assert_eq!(fresh_token(cached, 1_000), Some("no-exp".into()));
    }
}
