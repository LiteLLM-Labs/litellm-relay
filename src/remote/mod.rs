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

use std::{
    io::{BufRead, Write},
    net::SocketAddr,
    path::PathBuf,
};

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
        #[arg(long)]
        claude_supervisor: Option<PathBuf>,
        #[arg(long, default_value = "litellm")]
        provider: String,
    },
    Handoff {
        session: Option<uuid::Uuid>,
        #[arg(long)]
        claude_supervisor: Option<PathBuf>,
    },
    #[command(hide = true)]
    ClaudeSupervisor {
        #[arg(long)]
        socket: Option<PathBuf>,
        #[arg(long)]
        claude_executable: Option<PathBuf>,
        #[arg(long)]
        idle_exit: bool,
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

fn choose_handoff(
    sessions: &[protocol::Session],
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> Result<uuid::Uuid> {
    anyhow::ensure!(
        !sessions.is_empty(),
        "No saved Claude conversations found. Exit Claude Code, then try again."
    );
    writeln!(
        output,
        "Choose a Claude conversation to continue on your phone."
    )?;
    for (index, session) in sessions.iter().enumerate() {
        let label: String = format!("{} ({})", session.title, session.project)
            .chars()
            .filter(|character| !character.is_control())
            .collect();
        let updated = jiff::Timestamp::from_second(session.updated_at)
            .map(|timestamp| timestamp.to_string())
            .unwrap_or_default();
        writeln!(output, "{}. {label}  {updated}", index + 1)?;
    }
    loop {
        write!(output, "Conversation number, or 0 to cancel ")?;
        output.flush()?;
        let mut line = String::new();
        anyhow::ensure!(
            input.read_line(&mut line)? > 0 && line.trim() != "0",
            "Handoff canceled."
        );
        if let Some(session) = line
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|choice| choice.checked_sub(1))
            .and_then(|index| sessions.get(index))
        {
            return protocol::split_session(&session.id)
                .map(|(_, id)| id)
                .map_err(|error| anyhow::anyhow!(error.error));
        }
        writeln!(output, "Choose one of the conversation numbers above.")?;
    }
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
            claude_supervisor,
            provider,
        } => {
            let credentials = connector::load()?;
            let _lock = crate::system::lock_private(
                &crate::config::relay_home().join("remote/connector.lock"),
            )?;
            let mut sessions = connector::sessions(
                &credentials,
                codex_socket,
                claude_executable.clone(),
                provider,
            );
            if cfg!(target_os = "linux") || claude_supervisor.is_some() {
                let auto_start = claude_supervisor.is_none();
                sessions.claude = claude::Backend::Remote(claude::supervisor::Client::new(
                    claude_supervisor.unwrap_or_else(|| {
                        claude::supervisor::default_socket(credentials.device_id)
                    }),
                    auto_start,
                    claude_executable,
                ));
            }
            connector::connect(credentials, sessions).await?;
        }
        Command::ClaudeSupervisor {
            socket,
            claude_executable,
            idle_exit,
        } => {
            let credentials = connector::load()?;
            let socket =
                socket.unwrap_or_else(|| claude::supervisor::default_socket(credentials.device_id));
            if let Some(server) = claude::supervisor::Server::bind(&socket)? {
                let sessions =
                    connector::sessions(&credentials, None, claude_executable, "litellm".into());
                let claude::Backend::Local(claude) = sessions.claude else {
                    unreachable!()
                };
                server.serve(std::sync::Arc::new(*claude), idle_exit, async {
                    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
                    tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
                }).await?;
            }
        }
        Command::Handoff {
            session,
            claude_supervisor,
        } => {
            let credentials = connector::load()?;
            let mut sessions = connector::sessions(&credentials, None, None, "litellm".into());
            let explicit = claude_supervisor.is_some();
            let socket = claude_supervisor
                .unwrap_or_else(|| claude::supervisor::default_socket(credentials.device_id));
            let client = claude::supervisor::Client::new(socket, false, None);
            match client.call(claude::supervisor::Request::Hello).await {
                Ok(_) => sessions.claude = claude::Backend::Remote(client),
                Err(error) if error.status == 503 && !explicit => {}
                Err(error) => return Err(anyhow::anyhow!(error.error)),
            }
            let session = match session {
                Some(session) => session,
                None => {
                    let choices = sessions
                        .claude
                        .handoff_choices()
                        .await
                        .map_err(|error| anyhow::anyhow!(error.error))?;
                    tokio::task::spawn_blocking(move || {
                        choose_handoff(
                            &choices,
                            &mut std::io::stdin().lock(),
                            &mut std::io::stdout().lock(),
                        )
                    })
                    .await??
                }
            };
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
            let path =
                state.unwrap_or_else(|| crate::config::relay_home().join("remote/control.sqlite"));
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
