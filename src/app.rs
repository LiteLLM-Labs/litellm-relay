use std::io::IsTerminal;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::{
    ai_tools::{
        autoconfigure, detect::AiTool, onboard, onboard_codex, onboard_desktop, print_codex_token,
        print_token, AutoConfigureParams, CodexOnboardParams, OnboardDesktopParams, OnboardParams,
    },
    cert::ensure_ca,
    config::{IdpOverrides, RelayConfig},
    pac::build_pac,
    proxy::RelayProxy,
    setup::run_setup,
};

#[derive(Parser)]
#[command(name = "relay")]
#[command(bin_name = "relay")]
#[command(about = "Local LiteLLM Gateway relay for AI app traffic")]
struct Cli {
    #[command(subcommand)]
    command: Option<CommandKind>,
}

#[derive(Args, Clone, Debug, Default)]
struct OidcArgs {
    /// OIDC issuer URL; Relay reads `<issuer>/.well-known/openid-configuration`.
    #[arg(long = "oidc-issuer")]
    issuer: Option<String>,
    /// Client id of the public (no secret) app registration Relay signs in as.
    #[arg(long = "oidc-client-id")]
    client_id: Option<String>,
    /// Space-separated scopes to request instead of the defaults
    /// (`openid profile email offline_access`, trimmed to what the IdP offers).
    #[arg(long = "oidc-scopes")]
    scopes: Option<String>,
    /// Fixed loopback port for the sign-in redirect; defaults to a random free port.
    #[arg(long = "oidc-redirect-port")]
    redirect_port: Option<u16>,
}

impl From<OidcArgs> for IdpOverrides {
    fn from(args: OidcArgs) -> Self {
        IdpOverrides {
            issuer: args.issuer,
            client_id: args.client_id,
            scopes: args.scopes,
            redirect_port: args.redirect_port,
        }
    }
}

#[derive(Subcommand)]
enum CommandKind {
    /// Run the local Relay proxy.
    Serve,
    /// Print the PAC file served by Relay.
    Pac,
    /// Create the local CA and print its path.
    CaPath,
    /// Configure Gateway URL and API key for Relay ingest.
    Setup {
        #[arg(long)]
        gateway_url: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
    },
    /// Detect the AI tools installed on this device and wire each one through
    /// the Gateway in one pass. Run automatically after `relay setup`; also
    /// usable standalone (e.g. from an MDM postinstall) with `--api-key` or
    /// the `--oidc-*` overrides. Unset fields fall back to the saved Relay
    /// config.
    Autoconfigure {
        #[arg(long)]
        gateway_url: Option<String>,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        env_key: Option<String>,
        #[command(flatten)]
        oidc: OidcArgs,
        /// Restrict the pass to specific tools (repeatable), e.g.
        /// `--only claude-desktop`. Accepts `claude-code`, `claude-desktop`,
        /// `codex`. Omit to configure every detected tool.
        #[arg(long, value_name = "TOOL")]
        only: Vec<String>,
    },
    /// Wire Claude Code to route through the Gateway via IdP sign-in.
    Onboard {
        #[arg(long)]
        gateway_url: Option<String>,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Static gateway key fallback for environments without an IdP.
        #[arg(long)]
        api_key: Option<String>,
        #[command(flatten)]
        oidc: OidcArgs,
    },
    /// Wire Claude Desktop (third-party mode) to route through the Gateway.
    ///
    /// Pass --oidc-client-id and --oidc-issuer for single sign-on (each
    /// developer signs in with their corporate account; no key on the
    /// device), or --api-key for a static Gateway key.
    OnboardClaudeDesktop {
        #[arg(long)]
        gateway_url: Option<String>,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        oidc_client_id: Option<String>,
        #[arg(long)]
        oidc_issuer: Option<String>,
        #[arg(long)]
        oidc_scopes: Option<String>,
        #[arg(long)]
        oidc_redirect_port: Option<u16>,
    },
    /// Print a valid IdP bearer token for Claude Code's apiKeyHelper.
    ClaudeToken,
    /// Wire Codex CLI to route through the Gateway via IdP sign-in.
    OnboardCodex {
        #[arg(long)]
        gateway_url: Option<String>,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Have Codex read the bearer key from this env var instead of the
        /// token helper hook (Relay's token command populates it).
        #[arg(long)]
        env_key: Option<String>,
        /// Static gateway key fallback for environments without an IdP.
        #[arg(long)]
        api_key: Option<String>,
        #[command(flatten)]
        oidc: OidcArgs,
    },
    /// Print a valid IdP bearer token for Codex's auth command hook.
    CodexToken,
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => run_interactive_default().await,
        Some(command) => run_command(command).await,
    }
}

async fn run_interactive_default() -> Result<()> {
    let mut config = RelayConfig::load()?;
    if config.gateway_api_key.is_none() {
        println!("LiteLLM Relay is not set up yet. Starting setup.");
        run_setup(None, None).await?;
        config = RelayConfig::load()?;
    }
    RelayProxy::new(config).serve_forever().await
}

async fn run_command(command: CommandKind) -> Result<()> {
    let config = RelayConfig::load()?;
    match command {
        CommandKind::Serve => RelayProxy::new(config).serve_forever().await,
        CommandKind::Pac => {
            print!("{}", build_pac(&config));
            Ok(())
        }
        CommandKind::CaPath => {
            let ca = ensure_ca(&config.mitm_ca_dir)?;
            println!("{}", ca.cert_path.display());
            Ok(())
        }
        CommandKind::Setup {
            gateway_url,
            api_key,
        } => run_setup(gateway_url, api_key).await,
        CommandKind::Autoconfigure {
            gateway_url,
            team,
            api_key,
            env_key,
            oidc,
            only,
        } => {
            let only = parse_only(&only)?;
            autoconfigure(
                AutoConfigureParams {
                    gateway_url,
                    team,
                    api_key,
                    env_key,
                    idp: oidc.into(),
                    explicit_api_key: false,
                    saved_key_refused: false,
                },
                &only,
            )
            .await
        }
        CommandKind::Onboard {
            gateway_url,
            team,
            model,
            api_key,
            oidc,
        } => onboard(OnboardParams {
            gateway_url,
            team,
            model,
            api_key,
            idp: oidc.into(),
            quiet: false,
        }),
        CommandKind::OnboardClaudeDesktop {
            gateway_url,
            team,
            api_key,
            model,
            oidc_client_id,
            oidc_issuer,
            oidc_scopes,
            oidc_redirect_port,
        } => onboard_desktop(OnboardDesktopParams {
            gateway_url,
            team,
            api_key,
            model,
            oidc_client_id,
            oidc_issuer,
            oidc_scopes,
            oidc_redirect_port,
            allow_sign_in: std::io::stderr().is_terminal(),
            quiet: false,
            reuse_saved_sso: false,
            saved_key_refused: false,
        }),
        CommandKind::ClaudeToken => print_token(),
        CommandKind::OnboardCodex {
            gateway_url,
            team,
            model,
            env_key,
            api_key,
            oidc,
        } => onboard_codex(CodexOnboardParams {
            gateway_url,
            team,
            model,
            env_key,
            api_key,
            idp: oidc.into(),
            quiet: false,
        }),
        CommandKind::CodexToken => print_codex_token(),
    }
}

/// Parse `--only` tool slugs into `AiTool`s, erroring on an unknown value so a
/// typo in an MDM/LaunchDaemon invocation fails loudly instead of silently
/// configuring nothing.
fn parse_only(values: &[String]) -> Result<Vec<AiTool>> {
    values
        .iter()
        .map(|value| {
            AiTool::from_slug(value).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown --only tool '{value}' (expected claude-code, claude-desktop, or codex)"
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overrides_of(args: &[&str]) -> IdpOverrides {
        let cli = Cli::try_parse_from(args).expect("the command line must parse");
        match cli.command.expect("a subcommand") {
            CommandKind::Onboard { oidc, .. }
            | CommandKind::OnboardCodex { oidc, .. }
            | CommandKind::Autoconfigure { oidc, .. } => oidc.into(),
            other => panic!("unexpected command {}", describe(&other)),
        }
    }

    fn describe(command: &CommandKind) -> &'static str {
        match command {
            CommandKind::Serve => "serve",
            CommandKind::Pac => "pac",
            CommandKind::CaPath => "ca-path",
            CommandKind::Setup { .. } => "setup",
            CommandKind::Autoconfigure { .. } => "autoconfigure",
            CommandKind::Onboard { .. } => "onboard",
            CommandKind::OnboardClaudeDesktop { .. } => "onboard-claude-desktop",
            CommandKind::ClaudeToken => "claude-token",
            CommandKind::OnboardCodex { .. } => "onboard-codex",
            CommandKind::CodexToken => "codex-token",
        }
    }

    #[test]
    fn should_accept_the_oidc_flags_on_every_onboarding_command() {
        for command in ["onboard", "onboard-codex", "autoconfigure"] {
            let overrides = overrides_of(&[
                "relay",
                command,
                "--oidc-issuer",
                "https://idp.example.com/v2.0",
                "--oidc-client-id",
                "relay-client",
                "--oidc-scopes",
                "openid email",
                "--oidc-redirect-port",
                "8765",
            ]);
            assert_eq!(
                overrides.issuer.as_deref(),
                Some("https://idp.example.com/v2.0")
            );
            assert_eq!(overrides.client_id.as_deref(), Some("relay-client"));
            assert_eq!(overrides.scopes.as_deref(), Some("openid email"));
            assert_eq!(overrides.redirect_port, Some(8765));
        }
    }

    #[test]
    fn should_leave_the_overrides_empty_when_no_oidc_flag_is_passed() {
        let overrides = overrides_of(&["relay", "onboard", "--team", "eng"]);
        assert_eq!(overrides, IdpOverrides::default());
    }

    #[test]
    fn should_accept_a_team_on_the_claude_desktop_onboard() {
        let cli = Cli::try_parse_from(["relay", "onboard-claude-desktop", "--team", "eng"])
            .expect("the command line must parse");
        match cli.command.expect("a subcommand") {
            CommandKind::OnboardClaudeDesktop { team, .. } => {
                assert_eq!(team.as_deref(), Some("eng"))
            }
            other => panic!("unexpected command {}", describe(&other)),
        }
    }

    #[test]
    fn should_refuse_the_removed_authorize_url_flag() {
        for command in ["onboard", "onboard-codex", "autoconfigure"] {
            let error = Cli::try_parse_from([
                "relay",
                command,
                "--authorize-url",
                "https://idp.example.com/authorize",
            ])
            .err()
            .expect("--authorize-url must be rejected");
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        }
    }
}
