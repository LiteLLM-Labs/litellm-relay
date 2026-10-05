//! Auto-configuration: detect the AI tools installed on this device and wire
//! each one onto the Gateway in a single pass. This is what makes Relay "opt
//! out" instead of "opt in" — installing Relay routes every recognized tool
//! through the Gateway automatically, rather than requiring the operator to run
//! a separate onboard command per tool per machine.

use std::future::Future;

use anyhow::Result;
use chrono::Utc;
use console::style;
use url::Url;

use crate::{
    ai_tools::{
        claude_cli::{onboard, OnboardParams},
        claude_desktop::{onboard_desktop, OnboardDesktopParams},
        codex::{onboard as onboard_codex, CodexOnboardParams},
        detect::{detect_all, AiTool, DetectContext, Detection},
    },
    config::{load_settings, IdpOverrides, RelaySettings},
    credential::{
        check_client, check_credential, expiry_state, CredentialCheck, ExpiryState, REENROLL_HINT,
    },
};

/// Overrides forwarded to each tool's onboarder. Every field is optional; when
/// unset the individual onboarders fall back to the saved Relay config, so a
/// managed `config.yaml` seeded by the MDM is enough to configure a device with
/// no flags at all.
#[derive(Debug, Default, Clone)]
pub struct AutoConfigureParams {
    pub gateway_url: Option<String>,
    pub team: Option<String>,
    /// Static Gateway key for tools without an IdP (Claude Desktop static mode,
    /// Codex static key).
    pub api_key: Option<String>,
    /// Codex-only: read the bearer key from this env var instead of the token
    /// helper hook.
    pub env_key: Option<String>,
    pub idp: IdpOverrides,
    /// Set by `autoconfigure` when `--api-key` was passed on the command line,
    /// so a key filled in by the saved-credential fallback does not read as an
    /// explicit static-key run.
    pub explicit_api_key: bool,
}

/// Whether the Gateway still accepts the static credential about to be written into tool configs.
#[derive(Clone, Debug, PartialEq)]
pub enum CredentialGate {
    NotStatic,
    Verified { expiry: ExpiryState },
    Restricted { detail: String },
    Rejected { detail: String },
    Unverifiable { gateway: String, detail: String },
}

/// Detect installed tools and onboard each one, continuing past any single
/// tool's failure so one misconfigured tool never blocks the rest. Returns an
/// error only if every detected tool failed to configure, or if the Gateway
/// does not accept the static credential that would be written.
///
/// `only` restricts the pass to specific tools (empty means every tool). This
/// lets the root-owned periodic agent handle just Claude Desktop (its managed
/// file lives under `/etc`) while the per-user agent handles the rest.
pub async fn autoconfigure(mut params: AutoConfigureParams, only: &[AiTool]) -> Result<()> {
    params.explicit_api_key = params.api_key.is_some();
    apply_credential_fallback(&mut params)?;
    let gate_params = params.clone();
    autoconfigure_with(
        &DetectContext::from_env(),
        params,
        only,
        move |tools: &[AiTool]| credential_gate(gate_params.clone(), tools.to_vec()),
        &mut configure_tool,
    )
    .await
}

/// When the caller supplies no explicit credential and the IdP that results
/// from the saved config plus the overrides is incomplete, reuse the saved
/// Gateway key so tools that accept a static credential (Codex, Claude
/// Desktop) still get wired up on non-SSO setups. A configured IdP is always
/// preferred and left untouched.
fn apply_credential_fallback(params: &mut AutoConfigureParams) -> Result<()> {
    if params.api_key.is_some() || params.env_key.is_some() {
        return Ok(());
    }
    params.api_key = saved_key_fallback(&params.idp, &load_settings()?);
    Ok(())
}

fn saved_key_fallback(overrides: &IdpOverrides, settings: &RelaySettings) -> Option<String> {
    let mut idp = settings.idp.clone();
    idp.apply(overrides);
    if idp.is_configured() {
        return None;
    }
    settings
        .gateway
        .api_key
        .clone()
        .filter(|key| !key.trim().is_empty())
}

/// The static Gateway key this pass would write into some tool config, if any.
/// An explicit --api-key is gated whenever any detected tool would write it:
/// Codex and Claude Code prefer it over the IdP, and Claude Desktop falls back
/// to the saved key whenever it is given no OIDC flags and has no saved SSO to
/// reuse, even when the saved config names an IdP.
fn static_key_in_play(
    params: &AutoConfigureParams,
    saved_key: Option<&str>,
    saved_desktop_sso: bool,
    tools: &[AiTool],
) -> Option<String> {
    let desktop_reuses_sso = saved_desktop_sso && desktop_reuse_saved_sso(params);
    let desktop_static = tools.contains(&AiTool::ClaudeDesktop)
        && params.idp.client_id.is_none()
        && params.idp.issuer.is_none()
        && !desktop_reuses_sso;
    let explicit_key_lands = desktop_static
        || tools
            .iter()
            .any(|tool| matches!(tool, AiTool::ClaudeCode | AiTool::Codex));
    let explicit = params
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty());
    if explicit_key_lands {
        if let Some(key) = explicit {
            return Some(key.to_string());
        }
    }
    if desktop_static {
        return saved_key
            .filter(|key| !key.trim().is_empty())
            .map(str::to_string);
    }
    None
}

/// Verify the static key against the Gateway before it lands in any tool config.
async fn credential_gate(
    params: AutoConfigureParams,
    tools: Vec<AiTool>,
) -> Result<CredentialGate> {
    let settings = load_settings()?;
    let Some(api_key) = static_key_in_play(
        &params,
        settings.gateway.api_key.as_deref(),
        settings.claude.desktop_sso.is_some(),
        &tools,
    ) else {
        return Ok(CredentialGate::NotStatic);
    };
    let gateway_url = params
        .gateway_url
        .clone()
        .unwrap_or_else(|| settings.gateway.url.clone());
    let gate = match check_credential(&check_client(), &gateway_url, &api_key).await {
        CredentialCheck::Valid => {
            let expires_at = settings
                .gateway
                .expires_at
                .filter(|_| settings.gateway.api_key.as_deref() == Some(api_key.as_str()));
            CredentialGate::Verified {
                expiry: expiry_state(expires_at, Utc::now()),
            }
        }
        CredentialCheck::Restricted { detail } => CredentialGate::Restricted { detail },
        CredentialCheck::Rejected { detail } => CredentialGate::Rejected { detail },
        CredentialCheck::Unverifiable { detail } => CredentialGate::Unverifiable {
            gateway: gateway_url,
            detail,
        },
    };
    Ok(gate)
}

/// Result of attempting to configure one detected tool.
struct Configured {
    tool: AiTool,
    outcome: Result<()>,
}

/// Testable core: detection context, credential gate, and per-tool configure
/// function are injected so unit tests can assert selection/reporting without
/// writing real tool config files or talking to a Gateway. The gate is only
/// awaited once a tool is detected, so a device with nothing to configure
/// never talks to the Gateway.
async fn autoconfigure_with<Fut>(
    ctx: &DetectContext,
    params: AutoConfigureParams,
    only: &[AiTool],
    gate: impl FnOnce(&[AiTool]) -> Fut,
    configure: &mut dyn FnMut(AiTool, &AutoConfigureParams) -> Result<()>,
) -> Result<()>
where
    Fut: Future<Output = Result<CredentialGate>>,
{
    let mut detected = detect_all(ctx);
    if !only.is_empty() {
        detected.retain(|detection| only.contains(&detection.tool));
    }
    if detected.is_empty() {
        println!(
            "No supported AI tools detected on this device. Relay will route them through the \
             Gateway automatically once Claude Code, Claude Desktop, or Codex is installed."
        );
        return Ok(());
    }

    println!(
        "{} {}",
        style("Auto-configuring AI tools →").bold(),
        style(gateway_host()).cyan().bold()
    );
    println!();

    let tools: Vec<AiTool> = detected.iter().map(|d| d.tool).collect();
    match gate(&tools).await? {
        CredentialGate::Restricted { detail } => {
            println!(
                "  {}  The Gateway accepts the stored credential but does not allow it on \
                 GET /v1/models: {detail}",
                style("!").yellow().bold()
            );
        }
        CredentialGate::Rejected { detail } => {
            println!(
                "  {}  The Gateway rejected the stored credential: {detail}",
                style("✗").red().bold()
            );
            println!("     {REENROLL_HINT}");
            anyhow::bail!("Gateway credential rejected; no AI tool was configured");
        }
        CredentialGate::Unverifiable { gateway, detail } => {
            println!(
                "  {}  Could not verify the Gateway credential against {gateway}: {detail}. \
                 Leaving AI tool configs untouched.",
                style("!").yellow().bold()
            );
            anyhow::bail!("Gateway credential could not be verified; no AI tool was configured");
        }
        CredentialGate::Verified {
            expiry: ExpiryState::ExpiringSoon { at },
        } => {
            println!(
                "  {}  Gateway credential expires at {}. {REENROLL_HINT}",
                style("!").yellow().bold(),
                at.to_rfc3339()
            );
        }
        CredentialGate::NotStatic | CredentialGate::Verified { .. } => {}
    }

    let results: Vec<Configured> = detected
        .iter()
        .map(|Detection { tool, .. }| Configured {
            tool: *tool,
            outcome: configure(*tool, &params),
        })
        .collect();

    for configured in &results {
        let label = style(configured.tool.label()).bold();
        match &configured.outcome {
            Ok(()) => println!("  {}  {label}", style("✓").green().bold()),
            Err(error) => println!(
                "  {}  {label} {} {}",
                style("–").yellow().bold(),
                style("—").dim(),
                style(error).dim(),
            ),
        }
    }

    let failures = results
        .iter()
        .filter(|configured| configured.outcome.is_err())
        .count();
    let configured = results.len() - failures;

    println!();
    let summary = format!(
        "Configured {configured} of {} detected tools.",
        results.len()
    );
    if failures == 0 {
        println!("{}", style(summary).green().bold());
    } else {
        println!("{}", style(summary).yellow());
    }

    if failures > 0 && configured == 0 {
        anyhow::bail!("failed to configure any detected AI tool");
    }
    Ok(())
}

/// The Gateway host shown in the summary header. Loads the resolved settings and
/// extracts the URL host, falling back to the raw URL when it can't be parsed.
fn gateway_host() -> String {
    let raw = match load_settings() {
        Ok(settings) => settings.gateway.url,
        Err(_) => return "the Gateway".to_string(),
    };
    Url::parse(&raw)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or(raw)
}

/// Dispatch a single detected tool to its onboarder, forwarding overrides.
fn configure_tool(tool: AiTool, params: &AutoConfigureParams) -> Result<()> {
    match tool {
        AiTool::ClaudeCode => onboard(OnboardParams {
            gateway_url: params.gateway_url.clone(),
            team: params.team.clone(),
            model: None,
            api_key: params.api_key.clone(),
            idp: params.idp.clone(),
            quiet: true,
        }),
        AiTool::Codex => onboard_codex(CodexOnboardParams {
            gateway_url: params.gateway_url.clone(),
            team: params.team.clone(),
            model: None,
            env_key: params.env_key.clone(),
            api_key: params.api_key.clone(),
            idp: params.idp.clone(),
            quiet: true,
        }),
        AiTool::ClaudeDesktop => onboard_desktop(OnboardDesktopParams {
            gateway_url: params.gateway_url.clone(),
            api_key: params.api_key.clone(),
            model: None,
            oidc_client_id: params.idp.client_id.clone(),
            oidc_issuer: params.idp.issuer.clone(),
            oidc_scopes: params.idp.scopes.clone(),
            oidc_redirect_port: params.idp.redirect_port,
            allow_sign_in: false,
            quiet: true,
            reuse_saved_sso: desktop_reuse_saved_sso(params),
        }),
    }
}

/// Whether a Claude Desktop pass may reuse the saved SSO settings. An explicit
/// `--api-key` is a human asking for a static key, which clears the saved SSO
/// like `onboard-claude-desktop --api-key` does; a key filled by the
/// saved-credential fallback keeps it.
fn desktop_reuse_saved_sso(params: &AutoConfigureParams) -> bool {
    !params.explicit_api_key
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use chrono::DateTime;
    use std::{
        cell::Cell,
        env, fs,
        path::{Path, PathBuf},
        pin::Pin,
    };

    fn temp_home(tag: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("relay-auto-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ctx(home: &Path) -> DetectContext {
        DetectContext {
            home: home.to_path_buf(),
            path_dirs: Vec::new(),
            app_dirs: Vec::new(),
        }
    }

    fn static_key_params() -> AutoConfigureParams {
        AutoConfigureParams {
            api_key: Some("sk-static".into()),
            ..AutoConfigureParams::default()
        }
    }

    fn gate(
        value: CredentialGate,
    ) -> impl FnOnce(&[AiTool]) -> Pin<Box<dyn Future<Output = Result<CredentialGate>>>> {
        move |_tools: &[AiTool]| Box::pin(async move { Ok(value) })
    }

    #[tokio::test]
    async fn should_not_consult_the_gateway_when_no_tool_is_detected() {
        let home = temp_home("no-probe");
        let probed = Cell::new(false);
        let gate = |_tools: &[AiTool]| async {
            probed.set(true);
            Ok(CredentialGate::Rejected {
                detail: "must never be asked".into(),
            })
        };

        let result =
            autoconfigure_with(&ctx(&home), static_key_params(), &[], gate, &mut |_, _| {
                Ok(())
            })
            .await;

        assert!(result.is_ok(), "{result:?}");
        assert!(
            !probed.get(),
            "the credential gate ran with nothing to configure"
        );
        let _ = fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn should_configure_tools_with_a_route_restricted_credential() {
        let home = temp_home("restricted");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();

        let mut seen: Vec<AiTool> = Vec::new();
        autoconfigure_with(
            &ctx(&home),
            static_key_params(),
            &[],
            gate(CredentialGate::Restricted {
                detail: "Virtual key is not allowed to call this route".into(),
            }),
            &mut |tool, params| {
                assert_eq!(params.api_key.as_deref(), Some("sk-static"));
                seen.push(tool);
                Ok(())
            },
        )
        .await
        .expect("a route-restricted key is still a live key");

        assert_eq!(seen, vec![AiTool::ClaudeCode, AiTool::Codex]);
        let _ = fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn should_configure_only_detected_tools() {
        let home = temp_home("selected");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();

        let mut seen: Vec<AiTool> = Vec::new();
        autoconfigure_with(
            &ctx(&home),
            AutoConfigureParams::default(),
            &[],
            gate(CredentialGate::NotStatic),
            &mut |tool, _| {
                seen.push(tool);
                Ok(())
            },
        )
        .await
        .unwrap();

        assert_eq!(seen, vec![AiTool::ClaudeCode, AiTool::Codex]);
        let _ = fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn should_configure_only_the_requested_tools() {
        let home = temp_home("only");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();

        let mut seen: Vec<AiTool> = Vec::new();
        autoconfigure_with(
            &ctx(&home),
            AutoConfigureParams::default(),
            &[AiTool::Codex],
            gate(CredentialGate::NotStatic),
            &mut |tool, _| {
                seen.push(tool);
                Ok(())
            },
        )
        .await
        .unwrap();

        assert_eq!(seen, vec![AiTool::Codex]);
        let _ = fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn should_continue_when_one_tool_fails() {
        let home = temp_home("partial");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();

        let mut attempted = 0;
        let result = autoconfigure_with(
            &ctx(&home),
            AutoConfigureParams::default(),
            &[],
            gate(CredentialGate::NotStatic),
            &mut |tool, _| {
                attempted += 1;
                match tool {
                    AiTool::ClaudeCode => Err(anyhow!("no IdP configured")),
                    _ => Ok(()),
                }
            },
        )
        .await;

        assert!(result.is_ok(), "one failure must not abort the run");
        assert_eq!(attempted, 2);
        let _ = fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn should_error_when_all_tools_fail() {
        let home = temp_home("allfail");
        fs::create_dir_all(home.join(".codex")).unwrap();

        let result = autoconfigure_with(
            &ctx(&home),
            AutoConfigureParams::default(),
            &[],
            gate(CredentialGate::NotStatic),
            &mut |_, _| Err(anyhow!("boom")),
        )
        .await;

        assert!(result.is_err());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn should_reuse_saved_sso_when_the_key_came_from_the_saved_config() {
        let params = AutoConfigureParams {
            api_key: Some("sk-saved".to_string()),
            ..Default::default()
        };
        assert!(desktop_reuse_saved_sso(&params));
    }

    #[test]
    fn should_drop_saved_sso_on_an_explicit_api_key() {
        let params = AutoConfigureParams {
            api_key: Some("sk-flag".to_string()),
            explicit_api_key: true,
            ..Default::default()
        };
        assert!(!desktop_reuse_saved_sso(&params));
    }

    #[tokio::test]
    async fn should_succeed_with_no_tools_detected() {
        let home = temp_home("none");
        let result = autoconfigure_with(
            &ctx(&home),
            AutoConfigureParams::default(),
            &[],
            gate(CredentialGate::NotStatic),
            &mut |_, _| Ok(()),
        )
        .await;
        assert!(result.is_ok());
        let _ = fs::remove_dir_all(&home);
    }

    fn settings(issuer: &str, client_id: &str, api_key: Option<&str>) -> RelaySettings {
        let mut settings = RelaySettings::default();
        settings.idp.issuer = issuer.into();
        settings.idp.client_id = client_id.into();
        settings.gateway.api_key = api_key.map(str::to_string);
        settings
    }

    #[test]
    fn should_fall_back_to_the_saved_key_only_without_a_usable_idp() {
        let saved_key = Some("sk-saved");
        let no_overrides = IdpOverrides::default();

        assert_eq!(
            saved_key_fallback(&no_overrides, &settings("", "", saved_key)),
            Some("sk-saved".into())
        );
        assert_eq!(
            saved_key_fallback(&no_overrides, &settings("", "", Some("  "))),
            None
        );
        assert_eq!(
            saved_key_fallback(
                &no_overrides,
                &settings("https://login.example.com", "client", saved_key)
            ),
            None
        );
    }

    #[tokio::test]
    async fn should_refuse_to_configure_any_tool_with_a_rejected_credential() {
        let home = temp_home("rejected");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();

        let mut attempted = 0;
        let result = autoconfigure_with(
            &ctx(&home),
            static_key_params(),
            &[],
            gate(CredentialGate::Rejected {
                detail: "Authentication Error - Expired Key".into(),
            }),
            &mut |_, _| {
                attempted += 1;
                Ok(())
            },
        )
        .await;

        let error = result.expect_err("a rejected credential must fail the run");
        assert!(error.to_string().contains("rejected"), "{error}");
        assert_eq!(attempted, 0, "no tool config may be written");
        let _ = fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn should_leave_tools_untouched_when_the_credential_cannot_be_verified() {
        let home = temp_home("unverifiable");
        fs::create_dir_all(home.join(".codex")).unwrap();

        let mut attempted = 0;
        let result = autoconfigure_with(
            &ctx(&home),
            static_key_params(),
            &[],
            gate(CredentialGate::Unverifiable {
                gateway: "http://127.0.0.1:1".into(),
                detail: "connection refused".into(),
            }),
            &mut |_, _| {
                attempted += 1;
                Ok(())
            },
        )
        .await;

        let error = result.expect_err("an unverifiable credential must fail the run");
        assert!(
            error.to_string().contains("could not be verified"),
            "{error}"
        );
        assert_eq!(attempted, 0, "no tool config may be written");
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn static_key_in_play_covers_the_desktop_saved_key_fallback() {
        let saved_idp_params = AutoConfigureParams::default();
        assert_eq!(
            static_key_in_play(
                &saved_idp_params,
                Some("sk-saved"),
                false,
                &[AiTool::ClaudeDesktop]
            ),
            Some("sk-saved".to_string()),
            "the saved key Claude Desktop falls back to must be gated when the IdP is only in the saved config"
        );
        assert_eq!(
            static_key_in_play(
                &saved_idp_params,
                Some("sk-saved"),
                false,
                &[AiTool::ClaudeCode]
            ),
            None,
            "no static key reaches Claude Code without an explicit key"
        );

        let oidc_params = AutoConfigureParams {
            idp: IdpOverrides {
                client_id: Some("client".into()),
                issuer: Some("https://issuer.example.com".into()),
                ..IdpOverrides::default()
            },
            ..AutoConfigureParams::default()
        };
        assert_eq!(
            static_key_in_play(
                &oidc_params,
                Some("sk-saved"),
                false,
                &[AiTool::ClaudeDesktop]
            ),
            None,
            "Claude Desktop given OIDC flags never touches the saved key"
        );

        assert_eq!(
            static_key_in_play(
                &static_key_params(),
                Some("sk-saved"),
                false,
                &[AiTool::Codex]
            ),
            Some("sk-static".to_string()),
            "an explicit static key is in play without OIDC flags"
        );

        let flag_and_saved_idp = AutoConfigureParams {
            api_key: Some("sk-flag".into()),
            ..saved_idp_params.clone()
        };
        assert_eq!(
            static_key_in_play(
                &flag_and_saved_idp,
                Some("sk-saved"),
                false,
                &[AiTool::ClaudeDesktop, AiTool::ClaudeCode]
            ),
            Some("sk-flag".to_string()),
            "the explicit key wins over the saved key for Claude Desktop"
        );
        assert_eq!(
            static_key_in_play(
                &flag_and_saved_idp,
                Some("sk-saved"),
                false,
                &[AiTool::Codex]
            ),
            Some("sk-flag".to_string()),
            "an explicit key with the IdP in the saved config still lands in Codex"
        );
        assert_eq!(
            static_key_in_play(
                &flag_and_saved_idp,
                Some("sk-saved"),
                false,
                &[AiTool::ClaudeCode]
            ),
            Some("sk-flag".to_string()),
            "an explicit key with the IdP in the saved config still lands in Claude Code"
        );

        let oidc_params_with_key = AutoConfigureParams {
            api_key: Some("sk-flag".into()),
            ..oidc_params.clone()
        };
        assert_eq!(
            static_key_in_play(
                &oidc_params_with_key,
                Some("sk-saved"),
                false,
                &[AiTool::ClaudeDesktop]
            ),
            None,
            "Claude Desktop on OIDC writes no static key, so nothing is gated"
        );

        assert_eq!(
            static_key_in_play(
                &AutoConfigureParams {
                    api_key: Some("   ".into()),
                    ..AutoConfigureParams::default()
                },
                None,
                false,
                &[AiTool::Codex]
            ),
            None,
            "a blank explicit key is not a credential"
        );

        assert_eq!(
            static_key_in_play(
                &AutoConfigureParams::default(),
                Some("  "),
                false,
                &[AiTool::ClaudeDesktop]
            ),
            None,
            "a blank saved key is not a credential"
        );
    }

    #[test]
    fn should_judge_the_idp_after_applying_the_overrides() {
        let saved_key = Some("sk-saved");
        let issuer_only = IdpOverrides {
            issuer: Some("https://login.example.com".into()),
            ..IdpOverrides::default()
        };
        let client_only = IdpOverrides {
            client_id: Some("client".into()),
            ..IdpOverrides::default()
        };

        assert_eq!(
            saved_key_fallback(&issuer_only, &settings("", "", saved_key)),
            Some("sk-saved".into())
        );
        assert_eq!(
            saved_key_fallback(&issuer_only, &settings("", "client", saved_key)),
            None
        );
        assert_eq!(
            saved_key_fallback(
                &client_only,
                &settings("https://login.example.com", "", saved_key)
            ),
            None
        );
    }

    #[test]
    fn static_key_in_play_skips_claude_desktop_when_it_reuses_the_saved_sso() {
        let fallback_key = AutoConfigureParams {
            api_key: Some("sk-saved".into()),
            explicit_api_key: false,
            ..AutoConfigureParams::default()
        };
        assert_eq!(
            static_key_in_play(
                &fallback_key,
                Some("sk-saved"),
                true,
                &[AiTool::ClaudeDesktop]
            ),
            None,
            "Claude Desktop rebuilding its saved SSO never embeds the leftover key"
        );
        assert_eq!(
            static_key_in_play(
                &AutoConfigureParams::default(),
                Some("sk-saved"),
                true,
                &[AiTool::ClaudeDesktop]
            ),
            None,
            "the saved-config IdP fallback to the saved key does not apply when saved SSO is reused"
        );
        assert_eq!(
            static_key_in_play(
                &fallback_key,
                Some("sk-saved"),
                true,
                &[AiTool::ClaudeDesktop, AiTool::Codex]
            ),
            Some("sk-saved".to_string()),
            "Codex in the same pass still writes the fallback key, so it stays gated"
        );
        assert_eq!(
            static_key_in_play(
                &AutoConfigureParams {
                    api_key: Some("sk-flag".into()),
                    explicit_api_key: true,
                    ..AutoConfigureParams::default()
                },
                Some("sk-saved"),
                true,
                &[AiTool::ClaudeDesktop]
            ),
            Some("sk-flag".to_string()),
            "an explicit key drops the saved SSO, so Claude Desktop writes it and it is gated"
        );
    }

    #[tokio::test]
    async fn should_configure_every_detected_tool_with_a_verified_credential() {
        let home = temp_home("verified");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();
        let expires_at = DateTime::parse_from_rfc3339("2026-09-22T22:27:58Z")
            .unwrap()
            .with_timezone(&Utc);

        for expiry in [
            ExpiryState::Unknown,
            ExpiryState::Ok { at: expires_at },
            ExpiryState::ExpiringSoon { at: expires_at },
            ExpiryState::Expired { at: expires_at },
        ] {
            let mut seen: Vec<AiTool> = Vec::new();
            autoconfigure_with(
                &ctx(&home),
                static_key_params(),
                &[],
                gate(CredentialGate::Verified { expiry }),
                &mut |tool, params| {
                    assert_eq!(params.api_key.as_deref(), Some("sk-static"));
                    seen.push(tool);
                    Ok(())
                },
            )
            .await
            .unwrap_or_else(|error| panic!("{expiry:?} must configure tools: {error}"));
            assert_eq!(seen, vec![AiTool::ClaudeCode, AiTool::Codex], "{expiry:?}");
        }
        let _ = fs::remove_dir_all(&home);
    }
}
