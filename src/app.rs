use std::{io::IsTerminal, process::ExitCode};

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

use crate::{
    ai_tools::{
        autoconfigure,
        credential::{
            daemon_answers, run_credential, run_sign_in, run_sign_out, Audience, Host,
            LAUNCH_AGENT_LABEL,
        },
        detect::AiTool,
        launch_agent::{host_plist, runs_as_agent, DaemonHost, Launchd},
        onboard, onboard_codex, onboard_desktop, print_codex_token, print_token,
        AutoConfigureParams, CodexOnboardParams, OnboardDesktopParams, OnboardParams,
    },
    cert::ensure_ca,
    config::{load_settings, IdpOverrides, RelayConfig, RelaySettings},
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
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Daemon(Box<CommandKind>),
    #[command(flatten)]
    Helper(HelperCommand),
}

/// Commands that talk to the running daemon over the broker socket. They
/// never read the Relay config, so a client can run them with nothing but
/// the executable path.
#[derive(Subcommand)]
enum HelperCommand {
    /// Print the Gateway bearer for the calling client (Claude Desktop's
    /// credential helper; Claude Code and Codex pass `--proxy`).
    Credential {
        /// Print the token the local inference proxy on 127.0.0.1 accepts
        /// instead of the Gateway bearer, which then stays in the daemon.
        #[arg(long)]
        proxy: bool,
    },
    /// Sign in to the IdP through the browser and keep the session in the daemon.
    SignIn,
    /// Forget the daemon's IdP session and delete its Gateway key.
    SignOut,
    /// Serve the Gateway's MCP tools to the calling client over stdio; every
    /// call goes to the daemon, which holds the credential and the catalog.
    Mcp,
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
    /// Print the macOS LaunchAgent plist that keeps `relay serve` running.
    LaunchAgent,
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

pub async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.command {
        None => run_interactive_default().await.map(|()| ExitCode::SUCCESS),
        Some(Command::Daemon(command)) => run_command(*command).await.map(|()| ExitCode::SUCCESS),
        Some(Command::Helper(HelperCommand::Credential { proxy })) => {
            Ok(run_credential(match proxy {
                true => Audience::LocalProxy,
                false => Audience::Gateway,
            }))
        }
        Some(Command::Helper(HelperCommand::SignIn)) => Ok(run_sign_in()),
        Some(Command::Helper(HelperCommand::SignOut)) => Ok(run_sign_out()),
        Some(Command::Helper(HelperCommand::Mcp)) => Ok(run_mcp().await),
    }
}

#[cfg(unix)]
async fn run_mcp() -> ExitCode {
    crate::mcp::stdio::run_mcp(crate::mcp::socket::socket_path()).await
}

#[cfg(not(unix))]
async fn run_mcp() -> ExitCode {
    eprintln!("relay mcp: the MCP relay needs the Relay daemon's Unix socket, which this platform does not have");
    ExitCode::FAILURE
}

async fn run_interactive_default() -> Result<()> {
    let mut settings = load_settings()?;
    if settings.gateway.api_key.is_none() {
        println!("LiteLLM Relay is not set up yet. Starting setup.");
        run_setup(None, None).await?;
        settings = load_settings()?;
    }
    if another_daemon_answers(Host::current(), || {
        daemon_answers(&crate::broker::socket_path())
    }) {
        let config = settings.to_config();
        println!(
            "LiteLLM Relay is already running in the background (dashboard: http://{}:{}/). \
             To watch traffic in this terminal instead, stop it first: \
             launchctl bootout gui/$(id -u)/{LAUNCH_AGENT_LABEL}",
            config.host, config.port
        );
        return Ok(());
    }
    serve(settings).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServeRefusal {
    DaemonAnswers,
    AgentLoaded,
}

fn another_daemon_answers(host: Host, answers: impl FnOnce() -> bool) -> bool {
    host == Host::MacOs && answers()
}

fn serve_refusal(
    host: Host,
    this_process_is_the_agent: bool,
    answers: impl FnOnce() -> bool,
    agent_loaded: impl FnOnce() -> bool,
) -> Option<ServeRefusal> {
    if host != Host::MacOs {
        return None;
    }
    if answers() {
        return Some(ServeRefusal::DaemonAnswers);
    }
    if !this_process_is_the_agent && agent_loaded() {
        return Some(ServeRefusal::AgentLoaded);
    }
    None
}

async fn run_command(command: CommandKind) -> Result<()> {
    let config = RelayConfig::load()?;
    match command {
        CommandKind::Serve => serve(load_settings()?).await,
        CommandKind::Pac => {
            print!("{}", build_pac(&config));
            Ok(())
        }
        CommandKind::LaunchAgent => {
            print!("{}", host_plist()?);
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

/// Runs the proxy together with the credential broker: the broker answers on
/// its own Unix socket, extends its Gateway key on a timer, and deletes that
/// key when the daemon is told to stop.
#[cfg(unix)]
async fn serve(settings: RelaySettings) -> Result<()> {
    use std::sync::Arc;

    use crate::broker::{socket, socket_path, Broker, Dependencies, TICK};
    use crate::mcp::{
        service::{watch_session, McpDependencies, McpService, SESSION_CHECK_INTERVAL},
        socket as mcp_socket,
        upstream::RmcpUpstream,
    };

    let path = socket_path();
    let refusal = serve_refusal(
        Host::current(),
        runs_as_agent(),
        || daemon_answers(&path),
        || Launchd.agent_loaded(),
    );
    match refusal {
        Some(ServeRefusal::DaemonAnswers) => anyhow::bail!(
            "Relay is already running: a daemon answers on {}; stop it before starting another \
             (launchctl bootout gui/$(id -u)/{LAUNCH_AGENT_LABEL} stops the LaunchAgent)",
            path.display()
        ),
        Some(ServeRefusal::AgentLoaded) => anyhow::bail!(
            "Relay is installed as the {LAUNCH_AGENT_LABEL} LaunchAgent, which launchd keeps \
             running; stop it before serving from a terminal \
             (launchctl bootout gui/$(id -u)/{LAUNCH_AGENT_LABEL})"
        ),
        None => {}
    }
    let broker = Arc::new(Broker::new(&settings, Dependencies::live()));
    let listener = socket::bind(&path)?;
    eprintln!("broker: listening on {}", path.display());
    let socket_task = tokio::spawn(socket::serve(
        Arc::clone(&broker),
        listener,
        socket::daemon_uid(),
    ));
    let ticker = tokio::spawn(tick_forever(Arc::clone(&broker), TICK));
    let mcp = Arc::new(McpService::new(
        Arc::clone(&broker),
        &settings.mcp,
        McpDependencies {
            upstream: Arc::new(RmcpUpstream::default()),
            settings: Box::new(crate::broker::FileSettings),
            clock: Box::new(crate::broker::SystemClock),
        },
    ));
    let proxy = RelayProxy::new(settings.to_config())
        .with_broker(Arc::clone(&broker))
        .with_mcp(Arc::clone(&mcp));
    let mcp_path = mcp_socket::socket_path();
    let mcp_listener = mcp_socket::bind(&mcp_path)?;
    eprintln!("mcp: listening on {}", mcp_path.display());
    let mcp_socket_task = tokio::spawn(mcp_socket::serve(
        Arc::clone(&mcp),
        mcp_listener,
        socket::daemon_uid(),
    ));
    let mcp_watcher = tokio::spawn(watch_session(Arc::clone(&mcp), SESSION_CHECK_INTERVAL));

    let outcome = tokio::select! {
        served = proxy.serve_forever() => served,
        joined = socket_task => match joined {
            Ok(served) => served,
            Err(error) => Err(anyhow::anyhow!("broker socket task stopped: {error}")),
        },
        joined = mcp_socket_task => match joined {
            Ok(served) => served,
            Err(error) => Err(anyhow::anyhow!("mcp socket task stopped: {error}")),
        },
        () = shutdown_signal() => {
            eprintln!("broker: stopping");
            Ok(())
        }
    };
    ticker.abort();
    mcp_watcher.abort();
    tokio::task::spawn_blocking(move || broker.shutdown()).await?;
    outcome
}

#[cfg(not(unix))]
async fn serve(settings: RelaySettings) -> Result<()> {
    RelayProxy::new(settings.to_config()).serve_forever().await
}

#[cfg(unix)]
async fn tick_forever(broker: std::sync::Arc<crate::broker::Broker>, every: std::time::Duration) {
    let mut interval = tokio::time::interval(every);
    interval.tick().await;
    loop {
        interval.tick().await;
        let broker = std::sync::Arc::clone(&broker);
        if tokio::task::spawn_blocking(move || broker.tick())
            .await
            .is_err()
        {
            eprintln!("broker: the renewal tick panicked; the next tick runs anyway");
        }
    }
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};

    let terminate = signal(SignalKind::terminate());
    match terminate {
        Ok(mut terminate) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
            }
        }
        Err(error) => {
            eprintln!("broker: cannot listen for SIGTERM ({error}); only Ctrl-C deletes the key");
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn daemon_command(args: &[&str]) -> CommandKind {
        let cli = Cli::try_parse_from(args).expect("the command line must parse");
        match cli.command.expect("a subcommand") {
            Command::Daemon(command) => *command,
            Command::Helper(_) => panic!("expected a daemon command"),
        }
    }

    fn overrides_of(args: &[&str]) -> IdpOverrides {
        match daemon_command(args) {
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
            CommandKind::LaunchAgent => "launch-agent",
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
    fn should_parse_the_helper_commands_next_to_the_daemon_ones() {
        for (args, expected) in [
            (["relay", "credential"], "credential"),
            (["relay", "sign-in"], "sign-in"),
            (["relay", "sign-out"], "sign-out"),
            (["relay", "mcp"], "mcp"),
        ] {
            let cli = Cli::try_parse_from(args).expect("the command line must parse");
            let parsed = match cli.command.expect("a subcommand") {
                Command::Helper(HelperCommand::Credential { proxy: false }) => "credential",
                Command::Helper(HelperCommand::Credential { proxy: true }) => "credential --proxy",
                Command::Helper(HelperCommand::SignIn) => "sign-in",
                Command::Helper(HelperCommand::SignOut) => "sign-out",
                Command::Helper(HelperCommand::Mcp) => "mcp",
                Command::Daemon(other) => describe(&other),
            };
            assert_eq!(parsed, expected);
        }
        assert_eq!(describe(&daemon_command(&["relay", "serve"])), "serve");
        assert_eq!(
            describe(&daemon_command(&["relay", "claude-token"])),
            "claude-token"
        );
    }

    #[test]
    fn should_refuse_a_second_daemon_only_on_macos_and_only_while_one_answers() {
        assert!(another_daemon_answers(Host::MacOs, || true));
        assert!(!another_daemon_answers(Host::MacOs, || false));
        assert!(!another_daemon_answers(Host::Other, || true));
        assert_eq!(
            serve_refusal(Host::MacOs, false, || true, || true),
            Some(ServeRefusal::DaemonAnswers)
        );
        assert_eq!(
            serve_refusal(Host::MacOs, false, || false, || true),
            Some(ServeRefusal::AgentLoaded)
        );
        assert_eq!(serve_refusal(Host::MacOs, true, || false, || true), None);
        assert_eq!(serve_refusal(Host::MacOs, false, || false, || false), None);
        assert_eq!(serve_refusal(Host::Other, false, || true, || true), None);
        assert_eq!(
            describe(&daemon_command(&["relay", "launch-agent"])),
            "launch-agent"
        );
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
        match daemon_command(&["relay", "onboard-claude-desktop", "--team", "eng"]) {
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
