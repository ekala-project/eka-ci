use anyhow::Context;
use octocrab::Octocrab;
use thiserror::Error;
use tracing::{info, warn};

use crate::config::GitHubAppConfig;

pub mod service;
mod webhook;

pub use service::{CICheckInfo, GitHubService, GitHubTask, JobDifference};
pub use webhook::handle_webhook_payload;

#[derive(Error, Debug)]
pub enum AppRegistrationError {
    #[error(transparent)]
    InvalidEnv(#[from] anyhow::Error),
    #[error("invalid value for $GITHUB_APP_ID")]
    InvalidAppId(#[from] std::num::ParseIntError),
    #[error("invalid value for $GITHUB_APP_PRIVATE_KEY")]
    InvalidPrivateKey(#[from] jsonwebtoken::errors::Error),
    #[error("")]
    Octocrab(#[from] octocrab::Error),
}

/// Register GitHub App from configuration
/// If config is provided, loads credentials from the configured source
/// Otherwise, falls back to environment variables for backward compatibility
pub async fn register_app_from_config(
    config: Option<&GitHubAppConfig>,
) -> Result<Octocrab, AppRegistrationError> {
    let (app_id, private_key) = if let Some(cfg) = config {
        info!("Registering GitHub App from configuration: {}", cfg.id);

        // Load credentials from configured source
        let credentials = cfg
            .credentials
            .load()
            .await
            .context("Failed to load GitHub App credentials")?;

        let app_id_str = credentials
            .get("GITHUB_APP_ID")
            .ok_or_else(|| anyhow::anyhow!("GITHUB_APP_ID not found in credentials"))?;

        let private_key = credentials
            .get("GITHUB_APP_PRIVATE_KEY")
            .ok_or_else(|| anyhow::anyhow!("GITHUB_APP_PRIVATE_KEY not found in credentials"))?
            .clone();

        (app_id_str.parse::<u64>()?.into(), private_key)
    } else {
        warn!("No GitHub App configuration found, falling back to environment variables");
        warn!("This fallback is deprecated. Please configure GitHub Apps in your config file.");

        let app_id = std::env::var("GITHUB_APP_ID")
            .context("failed to locate $GITHUB_APP_ID")?
            .parse::<u64>()?
            .into();

        let private_key = std::env::var("GITHUB_APP_PRIVATE_KEY")
            .context("failed to locate $GITHUB_APP_PRIVATE_KEY")?;

        (app_id, private_key)
    };

    let key = jsonwebtoken::EncodingKey::from_rsa_pem(private_key.as_bytes())?;
    let octocrab = Octocrab::builder().app(app_id, key).build()?;

    info!("Successfully registered as GitHub app");

    Ok(octocrab)
}
