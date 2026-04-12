mod auth;
mod cache_permissions;
mod checks;
mod ci;
mod client;
mod config;
mod db;
mod git;
mod github;
mod github_permissions;
mod graph;
mod hooks;
mod metrics;
mod nix;
mod scheduler;
mod services;
mod web;
mod webhook_security;

#[cfg(test)]
mod tests;

use config::Config;
use rustls::crypto::{CryptoProvider, ring};
use tracing::debug;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize Rustls crypto provider (required for reqwest/octocrab TLS)
    // This must be done before any HTTP clients are created
    let _ = CryptoProvider::install_default(ring::default_provider());

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
