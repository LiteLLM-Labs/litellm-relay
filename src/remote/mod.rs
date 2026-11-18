mod auth;
mod claude;
mod codex;
mod connector;
mod peers;
mod protocol;
mod route;
mod service;
mod sessions;
mod store;

#[cfg(test)]
#[path = "../../litellm-remote/tests/remote.rs"]
mod tests;

use std::{net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result};
use clap::Subcommand;
use url::Url;

#[derive(Subcommand)]
pub enum Command {
    Pair {
        #[arg(long)]
        url: Option<Url>,
        #[arg(long)]
        name: Option<String>,
    },
    Connect {
        #[arg(long)]
        codex_socket: Option<PathBuf>,
        #[arg(long)]
        claude_executable: Option<PathBuf>,
        #[arg(long, default_value = "litellm")]
        provider: String,
    },
    Handoff {
        session: uuid::Uuid,
    },
    Serve {
        #[arg(long)]
        gateway_url: Option<Url>,
        #[arg(long)]
        public_url: Option<Url>,
        #[arg(long, default_value = "127.0.0.1:4174")]
        listen: SocketAddr,
        #[arg(long)]
        state: Option<PathBuf>,
    },
}

fn address(argument: Option<Url>, variable: &str) -> Result<Url> {
    let url = match argument {
        Some(url) => url,
        None => std::env::var(variable)
            .ok()
            .and_then(|value| Url::parse(&value).ok())
            .with_context(|| format!("Set {variable} or pass its command-line option."))?,
    };
    let local = matches!(
        url.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]" | "::1")
    );
    anyhow::ensure!(
        (url.scheme() == "https" || (local && url.scheme() == "http"))
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Use HTTPS without credentials, a query, or a fragment. HTTP is allowed only on loopback."
    );
    Ok(url)
}

pub async fn run(command: Command) -> Result<()> {
    match command {
        Command::Pair { url, name } => {
            let endpoint = address(url, "LITELLM_REMOTE_URL")?;
            anyhow::ensure!(
                endpoint.path() == "/",
                "Use the remote service origin without a path."
            );
            connector::pair(endpoint, name).await?;
        }
        Command::Connect {
            codex_socket,
            claude_executable,
            provider,
        } => {
            let credentials = connector::load()?;
            let _lock = crate::system::lock_private(
                &crate::config::relay_home().join("remote/connector.lock"),
            )?;
            let sessions =
                connector::sessions(&credentials, codex_socket, claude_executable, provider);
            connector::connect(credentials, sessions).await?;
        }
        Command::Handoff { session } => {
            let credentials = connector::load()?;
            let sessions = connector::sessions(&credentials, None, None, "litellm".into());
            sessions
                .claude
                .handoff(session)
                .await
                .map_err(|error| anyhow::anyhow!(error.error))?;
            println!(
                "The saved Claude session is ready in LiteLLM Remote. Keep the connector running."
            );
        }
        Command::Serve {
            gateway_url,
            public_url,
            listen,
            state,
        } => {
            let configured_gateway = address(gateway_url, "LITELLM_GATEWAY_URL")?;
            let gateway = Url::parse(route::normalized(&configured_gateway))?;
            let public = address(public_url, "REMOTE_PUBLIC_URL")?;
            anyhow::ensure!(
                public.path() == "/",
                "Use the control service origin without a path."
            );
            let path = state
                .or_else(|| std::env::var_os("REMOTE_STATE_PATH").map(PathBuf::from))
                .unwrap_or_else(|| crate::config::relay_home().join("remote/control.sqlite"));
            let service = service::Service::open(gateway, public, &path)
                .map_err(|error| anyhow::anyhow!(error.error))?;
            let listener = tokio::net::TcpListener::bind(listen)
                .await
                .context("Cannot bind the remote-control service.")?;
            println!("LiteLLM Remote is listening on {}", listener.local_addr()?);
            axum::serve(
                listener,
                service
                    .router()
                    .into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        }
    }
    Ok(())
}
