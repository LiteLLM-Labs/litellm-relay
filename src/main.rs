mod ai_tools;
mod app;
mod apps;
mod auth;
mod broker;
mod cert;
mod config;
mod credential;
mod events;
mod gateway;
mod http;
mod pac;
mod proxy;
mod setup;
mod system;
mod terminal;
mod traffic;

#[tokio::main]
async fn main() -> anyhow::Result<std::process::ExitCode> {
    #[cfg(not(test))]
    cert::install_default_crypto_provider();
    app::run().await
}
