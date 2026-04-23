mod auth;
mod cache_permissions;
mod checks;
mod ci;
mod client;
mod config;
mod db;
mod dependency_comparison;
mod git;
mod github;
mod github_permissions;
mod graph;
mod hooks;
mod metrics;
mod nix;
mod path_safety;
mod scheduler;
mod secret;
mod services;
mod web;
mod webhook_security;

#[cfg(test)]
mod tests;

use config::Config;
use rustls::crypto::{CryptoProvider, ring};
use tracing::info;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize Rustls crypto provider (required for reqwest/octocrab TLS).
    // This must be done before any HTTP clients are created. The call
    // returns `Err` only when a default provider has already been
    // installed in-process, which for this binary means a second call
    // from the same process — benign and ignored. (No tracing yet;
    // `tracing_subscriber` is initialised below.)
    let _already_installed = CryptoProvider::install_default(ring::default_provider());

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
    // M2: we intentionally do NOT log the full `Config` here (or
    // anywhere else) because it embeds OAuth secrets, the JWT secret,
    // the webhook secret, and cache credentials. Individual settings
    // are logged at the point they take effect.
    info!("Configuration loaded");

    config.ensure_logs_dir()?;

    services::start_services(config).await?;
    Ok(())
}
