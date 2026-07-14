use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::{
    ai_tools::{onboard, onboard_desktop, print_token, OnboardDesktopParams, OnboardParams},
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
    },
    /// Print a valid IdP bearer token for Claude Code's apiKeyHelper.
    ClaudeToken,
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
        CommandKind::Onboard {
            gateway_url,
            authorize_url,
            team,
            model,
        } => onboard(OnboardParams {
            gateway_url,
            authorize_url,
            team,
            model,
        }),
        CommandKind::OnboardClaudeDesktop {
            gateway_url,
            api_key,
            model,
            oidc_client_id,
            oidc_issuer,
            oidc_scopes,
            oidc_redirect_port,
        } => onboard_desktop(OnboardDesktopParams {
            gateway_url,
            api_key,
            model,
            oidc_client_id,
            oidc_issuer,
            oidc_scopes,
            oidc_redirect_port,
        }),
        CommandKind::ClaudeToken => print_token(),
    }
}
