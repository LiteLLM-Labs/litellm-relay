use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::{
    ai_tools::{
        autoconfigure, detect::AiTool, onboard, onboard_codex, onboard_desktop, print_codex_token,
        print_token, AutoConfigureParams, CodexOnboardParams, OnboardDesktopParams, OnboardParams,
    },
    cert::ensure_ca,
    config::RelayConfig,
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
    /// usable standalone (e.g. from an MDM postinstall) with `--api-key` /
    /// `--authorize-url` / OIDC overrides. Unset fields fall back to the saved
    /// Relay config.
    Autoconfigure {
        #[arg(long)]
        gateway_url: Option<String>,
        #[arg(long)]
        authorize_url: Option<String>,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        env_key: Option<String>,
        #[arg(long)]
        oidc_client_id: Option<String>,
        #[arg(long)]
        oidc_issuer: Option<String>,
        #[arg(long)]
        oidc_scopes: Option<String>,
        #[arg(long)]
        oidc_redirect_port: Option<u16>,
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
        authorize_url: Option<String>,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// Static gateway key fallback for environments without an IdP.
        #[arg(long)]
        api_key: Option<String>,
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
        /// Write the root-owned managed-settings file (MDM / fleet deploy)
        /// instead of the per-user config. Requires sudo. Defaults to the
        /// per-user config (no sudo).
        #[arg(long)]
        managed: bool,
    },
    /// Print a valid IdP bearer token for Claude Code's apiKeyHelper.
    ClaudeToken,
    /// Wire Codex CLI to route through the Gateway via IdP sign-in.
    OnboardCodex {
        #[arg(long)]
        gateway_url: Option<String>,
        #[arg(long)]
        authorize_url: Option<String>,
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
            authorize_url,
            team,
            api_key,
            env_key,
            oidc_client_id,
            oidc_issuer,
            oidc_scopes,
            oidc_redirect_port,
            only,
        } => {
            let only = parse_only(&only)?;
            autoconfigure(
                AutoConfigureParams {
                    gateway_url,
                    authorize_url,
                    team,
                    api_key,
                    env_key,
                    oidc_client_id,
                    oidc_issuer,
                    oidc_scopes,
                    oidc_redirect_port,
                },
                &only,
            )
        }
        CommandKind::Onboard {
            gateway_url,
            authorize_url,
            team,
            model,
            api_key,
        } => onboard(OnboardParams {
            gateway_url,
            authorize_url,
            team,
            model,
            api_key,
            quiet: false,
        }),
        CommandKind::OnboardClaudeDesktop {
            gateway_url,
            api_key,
            model,
            oidc_client_id,
            oidc_issuer,
            oidc_scopes,
            oidc_redirect_port,
            managed,
        } => onboard_desktop(OnboardDesktopParams {
            gateway_url,
            api_key,
            model,
            oidc_client_id,
            oidc_issuer,
            oidc_scopes,
            oidc_redirect_port,
            managed,
            quiet: false,
        }),
        CommandKind::ClaudeToken => print_token(),
        CommandKind::OnboardCodex {
            gateway_url,
            authorize_url,
            team,
            model,
            env_key,
            api_key,
        } => onboard_codex(CodexOnboardParams {
            gateway_url,
            authorize_url,
            team,
            model,
            env_key,
            api_key,
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
