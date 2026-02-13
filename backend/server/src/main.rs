mod auth;
mod checks;
mod ci;
mod client;
mod config;
mod db;
mod git;
mod github;
mod metrics;
mod nix;
mod scheduler;
mod services;
mod web;

#[cfg(test)]
mod tests;

use config::Config;
use tracing::debug;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .with_ansi(true)
        .with_level(true)
        .with_target(true)
        .with_timer(tracing_subscriber::fmt::time())
        .init();

    let config = Config::from_env()?;
    debug!("Using configuration {config:?}");

    config.ensure_logs_dir()?;

    services::start_services(config).await?;
    Ok(())
}
