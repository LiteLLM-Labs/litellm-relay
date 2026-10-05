use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    ai_tools::idp::{self, token_expiry, Session},
    auth::open_browser,
    config::{relay_home, IdpSection},
};

const REFRESH_AHEAD_SECONDS: i64 = 600;
const EXPIRY_MARGIN_SECONDS: i64 = 60;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
struct CachedSession {
    issuer: String,
    client_id: String,
    token: String,
    exp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Reuse(String),
    Refresh {
        refresh_token: String,
        still_valid: Option<String>,
    },
    SignIn,
}

/// Returns a valid IdP ID token for any onboarded tool. Reuses the cached token
/// until it nears expiry, then renews it silently with the refresh token and
/// only falls back to a browser sign-in when no refresh is possible. The token
/// is identity-scoped, so it is shared across tools.
pub fn ensure_token(idp: &IdpSection) -> Result<String> {
    ensure_with(
        &token_cache_path(),
        idp,
        Utc::now().timestamp(),
        &open_browser,
    )
}

fn ensure_with(path: &Path, idp: &IdpSection, now: i64, browser: &dyn Fn(&str)) -> Result<String> {
    if !idp.is_configured() {
        bail!("no IdP configured; {}", idp.setup_hint());
    }
    if let Step::Reuse(token) = next_step(read_cache(path, idp)?, now) {
        return Ok(token);
    }
    let _lock = lock_cache(path)?;
    match next_step(read_cache(path, idp)?, now) {
        Step::Reuse(token) => Ok(token),
        Step::SignIn => store(path, idp, idp::sign_in(idp, browser)?),
        Step::Refresh {
            refresh_token,
            still_valid,
        } => match (idp::refresh(idp, &refresh_token), still_valid) {
            (Ok(session), _) => store(path, idp, session),
            (Err(error), Some(token)) => {
                eprintln!(
                    "Silent token refresh failed ({error:#}); using the current token until it expires."
                );
                Ok(token)
            }
            (Err(error), None) => {
                eprintln!("Silent token refresh failed ({error:#}); signing in again.");
                store(path, idp, idp::sign_in(idp, browser)?)
            }
        },
    }
}

fn next_step(cached: Option<CachedSession>, now: i64) -> Step {
    let Some(cached) = cached else {
        return Step::SignIn;
    };
    let remaining = cached.exp - now;
    match cached.refresh_token {
        Some(_) if remaining > REFRESH_AHEAD_SECONDS => Step::Reuse(cached.token),
        Some(refresh_token) => Step::Refresh {
            refresh_token,
            still_valid: (remaining > EXPIRY_MARGIN_SECONDS).then_some(cached.token),
        },
        None if remaining > EXPIRY_MARGIN_SECONDS => Step::Reuse(cached.token),
        None => Step::SignIn,
    }
}

fn read_cache(path: &Path, idp: &IdpSection) -> Result<Option<CachedSession>> {
    if !path.exists() {
        return Ok(None);
    }
    let contents =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(serde_json::from_str::<CachedSession>(&contents)
        .ok()
        .filter(|cached| {
            cached.issuer == idp.normalized_issuer() && cached.client_id == idp.client_id.trim()
        }))
}

fn store(path: &Path, idp: &IdpSection, session: Session) -> Result<String> {
    let exp = token_expiry(&session.id_token)
        .context("the IdP issued an ID token without a readable exp claim")?;
    let cached = CachedSession {
        issuer: idp.normalized_issuer().to_string(),
        client_id: idp.client_id.trim().to_string(),
        token: session.id_token,
        exp,
        refresh_token: session.refresh_token,
    };
    write_cache(path, &cached)?;
    Ok(cached.token)
}

fn write_cache(path: &Path, cached: &CachedSession) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let staged = path.with_extension("json.tmp");
    let _ = fs::remove_file(&staged);
    private_file()
        .create_new(true)
        .open(&staged)
        .and_then(|mut file| file.write_all(serde_json::to_string(cached)?.as_bytes()))
        .with_context(|| format!("failed to write {}", staged.display()))?;
    fs::rename(&staged, path).with_context(|| format!("failed to replace {}", path.display()))
}

fn lock_cache(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let lock_path = path.with_extension("lock");
    let file = private_file()
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open {}", lock_path.display()))?;
    file.lock()
        .with_context(|| format!("failed to lock {}", lock_path.display()))?;
    Ok(file)
}

fn private_file() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn token_cache_path() -> PathBuf {
    relay_home().join("identity-token.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_tools::idp::test_support::*;
    use std::{env, sync::Barrier, thread, time::Duration};
    use uuid::Uuid;

    const NOW: i64 = 1_800_000_000;

    fn cache_path(tag: &str) -> PathBuf {
        env::temp_dir()
            .join(format!("relay-token-{tag}-{}", Uuid::new_v4().simple()))
            .join("identity-token.json")
    }

    fn cached(idp: &IdpSection, exp: i64, refresh_token: Option<&str>) -> CachedSession {
        CachedSession {
            issuer: idp.normalized_issuer().to_string(),
            client_id: idp.client_id.trim().to_string(),
            token: jwt_with_exp(exp),
            exp,
            refresh_token: refresh_token.map(str::to_string),
        }
    }

    fn unreachable_idp() -> IdpSection {
        IdpSection {
            issuer: "http://127.0.0.1:1".into(),
            client_id: CLIENT_ID.into(),
            ..IdpSection::default()
        }
    }

    fn panicking_browser(_: &str) {
        panic!("the browser must not open");
    }

    #[test]
    fn should_reuse_a_token_that_is_not_near_expiry() {
        let idp = unreachable_idp();
        let session = cached(&idp, NOW + REFRESH_AHEAD_SECONDS + 1, Some("refresh"));
        assert_eq!(
            next_step(Some(session), NOW),
            Step::Reuse(jwt_with_exp(NOW + REFRESH_AHEAD_SECONDS + 1))
        );
    }

    #[test]
    fn should_refresh_ahead_of_expiry_while_the_token_is_still_valid() {
        let idp = unreachable_idp();
        let session = cached(&idp, NOW + REFRESH_AHEAD_SECONDS, Some("refresh"));
        assert_eq!(
            next_step(Some(session), NOW),
            Step::Refresh {
                refresh_token: "refresh".into(),
                still_valid: Some(jwt_with_exp(NOW + REFRESH_AHEAD_SECONDS)),
            }
        );
    }

    #[test]
    fn should_refresh_without_a_fallback_once_the_token_is_about_to_expire() {
        let idp = unreachable_idp();
        let session = cached(&idp, NOW + EXPIRY_MARGIN_SECONDS, Some("refresh"));
        assert_eq!(
            next_step(Some(session), NOW),
            Step::Refresh {
                refresh_token: "refresh".into(),
                still_valid: None,
            }
        );
    }

    #[test]
    fn should_reuse_a_token_without_a_refresh_token_until_the_expiry_margin() {
        let idp = unreachable_idp();
        assert_eq!(
            next_step(
                Some(cached(&idp, NOW + EXPIRY_MARGIN_SECONDS + 1, None)),
                NOW
            ),
            Step::Reuse(jwt_with_exp(NOW + EXPIRY_MARGIN_SECONDS + 1))
        );
        assert_eq!(
            next_step(Some(cached(&idp, NOW + EXPIRY_MARGIN_SECONDS, None)), NOW),
            Step::SignIn
        );
        assert_eq!(
            next_step(Some(cached(&idp, NOW - 1, None)), NOW),
            Step::SignIn
        );
    }

    #[test]
    fn should_sign_in_without_a_cache() {
        assert_eq!(next_step(None, NOW), Step::SignIn);
    }

    #[test]
    fn should_round_trip_the_session_through_a_private_cache_file() {
        let path = cache_path("roundtrip");
        let idp = unreachable_idp();
        let stored = store(
            &path,
            &idp,
            Session {
                id_token: jwt_with_exp(NOW + 3600),
                refresh_token: Some("refresh-1".into()),
            },
        )
        .unwrap();

        assert_eq!(stored, jwt_with_exp(NOW + 3600));
        assert_eq!(
            read_cache(&path, &idp).unwrap(),
            Some(cached(&idp, NOW + 3600, Some("refresh-1")))
        );
        assert!(!path.with_extension("json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn should_replace_an_existing_cache_file_in_place() {
        let path = cache_path("replace");
        let idp = unreachable_idp();
        write_cache(&path, &cached(&idp, NOW + 10, Some("refresh-1"))).unwrap();

        store(
            &path,
            &idp,
            Session {
                id_token: jwt_with_exp(NOW + 3600),
                refresh_token: None,
            },
        )
        .unwrap();

        assert_eq!(
            read_cache(&path, &idp).unwrap(),
            Some(cached(&idp, NOW + 3600, None))
        );
    }

    #[test]
    fn should_refuse_to_cache_an_id_token_without_an_expiry() {
        let path = cache_path("no-exp");
        let idp = unreachable_idp();

        let error = store(
            &path,
            &idp,
            Session {
                id_token: "eyJhbGciOiJub25lIn0.eyJzdWIiOiJhIn0.".into(),
                refresh_token: None,
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("exp"), "{error:#}");
        assert!(!path.exists());
    }

    #[test]
    fn should_ignore_a_cache_written_by_an_older_relay() {
        let path = cache_path("legacy");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{\"token\":\"old\",\"exp\":123}").unwrap();

        assert_eq!(read_cache(&path, &unreachable_idp()).unwrap(), None);
    }

    #[test]
    fn should_ignore_a_corrupt_cache() {
        let path = cache_path("corrupt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not json").unwrap();

        assert_eq!(read_cache(&path, &unreachable_idp()).unwrap(), None);
    }

    #[test]
    fn should_ignore_a_cache_for_another_issuer_or_client() {
        let path = cache_path("other");
        let idp = unreachable_idp();
        write_cache(&path, &cached(&idp, NOW + 3600, Some("refresh-1"))).unwrap();
        let other_issuer = IdpSection {
            issuer: "http://127.0.0.1:2".into(),
            ..idp.clone()
        };
        let other_client = IdpSection {
            client_id: "another-client".into(),
            ..idp.clone()
        };

        assert_eq!(read_cache(&path, &other_issuer).unwrap(), None);
        assert_eq!(read_cache(&path, &other_client).unwrap(), None);
        assert!(read_cache(&path, &idp).unwrap().is_some());
    }

    #[test]
    fn should_match_the_cache_on_the_normalized_issuer_and_client_id() {
        let path = cache_path("normalized");
        let idp = unreachable_idp();
        write_cache(&path, &cached(&idp, NOW + 3600, None)).unwrap();
        let spaced = IdpSection {
            issuer: format!(" {}/ ", idp.issuer),
            client_id: format!(" {CLIENT_ID} "),
            ..idp.clone()
        };

        assert!(read_cache(&path, &spaced).unwrap().is_some());
    }

    #[test]
    fn should_return_a_fresh_cached_token_without_touching_the_idp() {
        let path = cache_path("fresh");
        let idp = unreachable_idp();
        write_cache(&path, &cached(&idp, NOW + 3600, Some("refresh-1"))).unwrap();

        let token = ensure_with(&path, &idp, NOW, &panicking_browser).unwrap();

        assert_eq!(token, jwt_with_exp(NOW + 3600));
    }

    #[test]
    fn should_refresh_silently_before_expiry() {
        let path = cache_path("refresh");
        let server = FakeIdp::start(
            None,
            vec![token_reply(&jwt_with_exp(NOW + 3600), Some("refresh-2"))],
        );
        let idp = server.idp();
        write_cache(&path, &cached(&idp, NOW + 300, Some("refresh-1"))).unwrap();

        let token = ensure_with(&path, &idp, NOW, &panicking_browser).unwrap();

        assert_eq!(token, jwt_with_exp(NOW + 3600));
        assert_eq!(
            read_cache(&path, &idp).unwrap(),
            Some(cached(&idp, NOW + 3600, Some("refresh-2")))
        );
        let requests = server.token_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["grant_type"], "refresh_token");
        assert_eq!(requests[0]["refresh_token"], "refresh-1");
    }

    #[test]
    fn should_keep_the_current_token_when_an_early_refresh_fails() {
        let path = cache_path("refresh-failed");
        let server = FakeIdp::start(
            None,
            vec![error_reply(
                503,
                "temporarily_unavailable",
                "try again later",
            )],
        );
        let idp = server.idp();
        write_cache(&path, &cached(&idp, NOW + 300, Some("refresh-1"))).unwrap();

        let token = ensure_with(&path, &idp, NOW, &panicking_browser).unwrap();

        assert_eq!(token, jwt_with_exp(NOW + 300));
        assert_eq!(
            read_cache(&path, &idp).unwrap(),
            Some(cached(&idp, NOW + 300, Some("refresh-1")))
        );
        assert_eq!(server.token_requests().len(), 1);
    }

    #[test]
    fn should_fall_back_to_the_browser_when_the_refresh_is_rejected_and_the_token_is_gone() {
        let path = cache_path("rejected");
        let server = FakeIdp::start(
            None,
            vec![
                error_reply(400, "invalid_grant", "refresh token revoked"),
                token_reply(&jwt_with_exp(NOW + 3600), Some("refresh-new")),
            ],
        );
        let idp = server.idp();
        write_cache(&path, &cached(&idp, NOW - 10, Some("refresh-stale"))).unwrap();
        let browser = FakeBrowser::approving();

        let token = ensure_with(&path, &idp, NOW, &browser.opener()).unwrap();

        assert_eq!(token, jwt_with_exp(NOW + 3600));
        assert_eq!(browser.seen.lock().unwrap().len(), 1);
        let requests = server.token_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["grant_type"], "refresh_token");
        assert_eq!(requests[1]["grant_type"], "authorization_code");
        assert_eq!(
            read_cache(&path, &idp).unwrap(),
            Some(cached(&idp, NOW + 3600, Some("refresh-new")))
        );
    }

    #[test]
    fn should_sign_in_through_the_browser_on_a_fresh_device() {
        let path = cache_path("first");
        let server = FakeIdp::start(
            None,
            vec![token_reply(&jwt_with_exp(NOW + 3600), Some("refresh-1"))],
        );
        let idp = server.idp();
        let browser = FakeBrowser::approving();

        let token = ensure_with(&path, &idp, NOW, &browser.opener()).unwrap();

        assert_eq!(token, jwt_with_exp(NOW + 3600));
        assert_eq!(
            read_cache(&path, &idp).unwrap(),
            Some(cached(&idp, NOW + 3600, Some("refresh-1")))
        );
    }

    #[test]
    fn should_sign_in_again_when_the_cache_belongs_to_another_idp() {
        let path = cache_path("switched");
        let server = FakeIdp::start(None, vec![token_reply(&jwt_with_exp(NOW + 3600), None)]);
        write_cache(
            &path,
            &cached(&unreachable_idp(), NOW + 3600, Some("refresh-elsewhere")),
        )
        .unwrap();
        let browser = FakeBrowser::approving();

        let token = ensure_with(&path, &server.idp(), NOW, &browser.opener()).unwrap();

        assert_eq!(token, jwt_with_exp(NOW + 3600));
        assert_eq!(browser.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn should_explain_how_to_configure_the_idp_even_when_a_cache_exists() {
        let path = cache_path("unconfigured");
        write_cache(&path, &cached(&unreachable_idp(), NOW + 3600, None)).unwrap();

        let error =
            ensure_with(&path, &IdpSection::default(), NOW, &panicking_browser).unwrap_err();

        assert!(error.to_string().contains("--oidc-issuer"), "{error:#}");
    }

    #[test]
    fn should_refresh_once_when_two_processes_race() {
        let path = cache_path("race");
        let server = FakeIdp::start_with(FakeIdpScript {
            token_replies: vec![token_reply(&jwt_with_exp(NOW + 3600), Some("refresh-2"))],
            token_delay: Duration::from_millis(300),
            ..FakeIdpScript::default()
        });
        let idp = server.idp();
        write_cache(&path, &cached(&idp, NOW + 300, Some("refresh-1"))).unwrap();
        let barrier = Barrier::new(2);

        let tokens: Vec<String> = thread::scope(|scope| {
            let workers: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        ensure_with(&path, &idp, NOW, &panicking_browser).unwrap()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect()
        });

        assert_eq!(tokens, vec![jwt_with_exp(NOW + 3600); 2]);
        assert_eq!(server.token_requests().len(), 1);
        assert_eq!(
            read_cache(&path, &idp).unwrap(),
            Some(cached(&idp, NOW + 3600, Some("refresh-2")))
        );
    }
}
