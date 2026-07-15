//! Auto-configuration: detect the AI tools installed on this device and wire
//! each one onto the Gateway in a single pass. This is what makes Relay "opt
//! out" instead of "opt in" — installing Relay routes every recognized tool
//! through the Gateway automatically, rather than requiring the operator to run
//! a separate onboard command per tool per machine.

use anyhow::Result;

use crate::{
    ai_tools::{
        claude_cli::{onboard, OnboardParams},
        claude_desktop::{onboard_desktop, OnboardDesktopParams},
        codex::{onboard as onboard_codex, CodexOnboardParams},
        detect::{detect_all, AiTool, DetectContext, Detection},
    },
    config::load_settings,
};

/// Overrides forwarded to each tool's onboarder. Every field is optional; when
/// unset the individual onboarders fall back to the saved Relay config, so a
/// managed `config.yaml` seeded by the MDM is enough to configure a device with
/// no flags at all.
#[derive(Debug, Default, Clone)]
pub struct AutoConfigureParams {
    pub gateway_url: Option<String>,
    pub authorize_url: Option<String>,
    pub team: Option<String>,
    /// Static Gateway key for tools without an IdP (Claude Desktop static mode,
    /// Codex static key).
    pub api_key: Option<String>,
    /// Codex-only: read the bearer key from this env var instead of the token
    /// helper hook.
    pub env_key: Option<String>,
    pub oidc_client_id: Option<String>,
    pub oidc_issuer: Option<String>,
    pub oidc_scopes: Option<String>,
    pub oidc_redirect_port: Option<u16>,
}

/// Detect installed tools and onboard each one, continuing past any single
/// tool's failure so one misconfigured tool never blocks the rest. Returns an
/// error only if every detected tool failed to configure.
pub fn autoconfigure(mut params: AutoConfigureParams) -> Result<()> {
    apply_credential_fallback(&mut params)?;
    autoconfigure_with(&DetectContext::from_env(), params, &mut configure_tool)
}

/// When the caller supplies no explicit credential and no IdP authorize URL is
/// configured, reuse the saved Gateway key so tools that accept a static
/// credential (Codex, Claude Desktop) still get wired up on non-SSO setups. A
/// configured IdP is always preferred and left untouched.
fn apply_credential_fallback(params: &mut AutoConfigureParams) -> Result<()> {
    if params.api_key.is_some() || params.env_key.is_some() || params.authorize_url.is_some() {
        return Ok(());
    }
    let settings = load_settings()?;
    if settings.idp.authorize_url.trim().is_empty() {
        params.api_key = settings
            .gateway
            .api_key
            .filter(|key| !key.trim().is_empty());
    }
    Ok(())
}

/// Result of attempting to configure one detected tool.
struct Configured {
    tool: AiTool,
    outcome: Result<()>,
}

/// Testable core: detection context and per-tool configure function are
/// injected so unit tests can assert selection/reporting without writing real
/// tool config files.
fn autoconfigure_with(
    ctx: &DetectContext,
    params: AutoConfigureParams,
    configure: &mut dyn FnMut(AiTool, &AutoConfigureParams) -> Result<()>,
) -> Result<()> {
    let detected = detect_all(ctx);
    if detected.is_empty() {
        println!(
            "No supported AI tools detected on this device. Relay will route them through the \
             Gateway automatically once Claude Code, Claude Desktop, or Codex is installed."
        );
        return Ok(());
    }

    println!(
        "Detected {} AI tool(s) to route through the Gateway:",
        detected.len()
    );
    for Detection { tool, evidence } in &detected {
        println!("  - {} ({evidence})", tool.label());
    }
    println!();

    let results: Vec<Configured> = detected
        .iter()
        .map(|Detection { tool, .. }| Configured {
            tool: *tool,
            outcome: configure(*tool, &params),
        })
        .collect();

    let failures: Vec<&Configured> = results
        .iter()
        .filter(|configured| configured.outcome.is_err())
        .collect();
    let configured = results.len() - failures.len();

    println!();
    println!(
        "Auto-configured {configured} of {} detected tool(s).",
        results.len()
    );
    for failure in &failures {
        if let Err(error) = &failure.outcome {
            eprintln!("  ! {} was not configured: {error:#}", failure.tool.label());
        }
    }

    if !failures.is_empty() && configured == 0 {
        anyhow::bail!("failed to configure any detected AI tool");
    }
    Ok(())
}

/// Dispatch a single detected tool to its onboarder, forwarding overrides.
fn configure_tool(tool: AiTool, params: &AutoConfigureParams) -> Result<()> {
    match tool {
        AiTool::ClaudeCode => onboard(OnboardParams {
            gateway_url: params.gateway_url.clone(),
            authorize_url: params.authorize_url.clone(),
            team: params.team.clone(),
            model: None,
        }),
        AiTool::Codex => onboard_codex(CodexOnboardParams {
            gateway_url: params.gateway_url.clone(),
            authorize_url: params.authorize_url.clone(),
            team: params.team.clone(),
            model: None,
            env_key: params.env_key.clone(),
            api_key: params.api_key.clone(),
        }),
        AiTool::ClaudeDesktop => onboard_desktop(OnboardDesktopParams {
            gateway_url: params.gateway_url.clone(),
            api_key: params.api_key.clone(),
            model: None,
            oidc_client_id: params.oidc_client_id.clone(),
            oidc_issuer: params.oidc_issuer.clone(),
            oidc_scopes: params.oidc_scopes.clone(),
            oidc_redirect_port: params.oidc_redirect_port,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::{
        env, fs,
        path::{Path, PathBuf},
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

    #[test]
    fn should_configure_only_detected_tools() {
        let home = temp_home("selected");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();

        let mut seen: Vec<AiTool> = Vec::new();
        autoconfigure_with(
            &ctx(&home),
            AutoConfigureParams::default(),
            &mut |tool, _| {
                seen.push(tool);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(seen, vec![AiTool::ClaudeCode, AiTool::Codex]);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn should_continue_when_one_tool_fails() {
        let home = temp_home("partial");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(home.join(".codex")).unwrap();

        let mut attempted = 0;
        let result = autoconfigure_with(
            &ctx(&home),
            AutoConfigureParams::default(),
            &mut |tool, _| {
                attempted += 1;
                match tool {
                    AiTool::ClaudeCode => Err(anyhow!("no IdP configured")),
                    _ => Ok(()),
                }
            },
        );

        assert!(result.is_ok(), "one failure must not abort the run");
        assert_eq!(attempted, 2);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn should_error_when_all_tools_fail() {
        let home = temp_home("allfail");
        fs::create_dir_all(home.join(".codex")).unwrap();

        let result =
            autoconfigure_with(&ctx(&home), AutoConfigureParams::default(), &mut |_, _| {
                Err(anyhow!("boom"))
            });

        assert!(result.is_err());
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn should_succeed_with_no_tools_detected() {
        let home = temp_home("none");
        let result =
            autoconfigure_with(&ctx(&home), AutoConfigureParams::default(), &mut |_, _| {
                Ok(())
            });
        assert!(result.is_ok());
        let _ = fs::remove_dir_all(&home);
    }
}
