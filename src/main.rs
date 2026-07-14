mod app;
mod apps;
mod auth;
mod cert;
mod claude;
mod config;
mod events;
mod gateway;
mod http;
mod idp;
mod pac;
mod proxy;
mod setup;
mod system;
mod terminal;
mod traffic;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(not(test))]
    cert::install_default_crypto_provider();
    app::run().await
}
