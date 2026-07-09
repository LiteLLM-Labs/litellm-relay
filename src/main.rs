mod app;
mod cert;
mod config;
mod events;
mod gateway;
mod http;
mod pac;
mod proxy;
mod setup;
mod system;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    app::run().await
}
