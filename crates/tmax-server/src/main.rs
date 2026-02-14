mod config;
mod connection;
mod server;

use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("tmax=info".parse()?))
        .init();

    let config = config::ServerConfig::load()?;
    server::run(config).await
}
