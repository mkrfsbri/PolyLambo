use anyhow::Result;

mod binance;
mod clob;
mod config;
mod engine;
mod gamma;
mod state;
mod tui;

#[tokio::main]
async fn main() -> Result<()> {
    let config = config::Config::from_env()?;
    tracing_subscriber::fmt()
        .with_env_filter(&config.log_level)
        .init();
    tracing::info!("eth5m-bot starting");
    Ok(())
}
