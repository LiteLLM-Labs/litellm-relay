use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::{
    ai_tools::{
        onboard, onboard_codex, print_codex_token, print_token, CodexOnboardParams, OnboardParams,
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
        #[arg(long)]
        wire_api: Option<String>,
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
        CommandKind::ClaudeToken => print_token(),
        CommandKind::OnboardCodex {
            gateway_url,
            authorize_url,
            team,
            model,
            wire_api,
            api_key,
        } => onboard_codex(CodexOnboardParams {
            gateway_url,
            authorize_url,
            team,
            model,
            wire_api,
            api_key,
        }),
        CommandKind::CodexToken => print_codex_token(),
    }
}
