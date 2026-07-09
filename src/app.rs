use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::{
    cert::ensure_ca, config::RelayConfig, pac::build_pac, proxy::RelayProxy, setup::run_setup,
};

#[derive(Parser)]
#[command(name = "litellm-relay")]
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
}

pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    let config = RelayConfig::from_env();
    match cli.command.unwrap_or(CommandKind::Serve) {
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
        } => run_setup(gateway_url, api_key),
    }
}
