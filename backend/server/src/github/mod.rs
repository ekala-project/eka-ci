use anyhow::Context;
use octocrab::Octocrab;
use thiserror::Error;
use tracing::info;

mod service;
mod webhook;

pub use service::{GitHubService, GitHubTask};
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

pub async fn register_app() -> Result<Octocrab, AppRegistrationError> {
    let app_id = std::env::var("GITHUB_APP_ID")
        .context("failed to locate $GITHUB_APP_ID")?
        .parse::<u64>()?
        .into();

    let app_private_key = std::env::var("GITHUB_APP_PRIVATE_KEY")
        .context("failed to locate $GITHUB_APP_PRIVATE_KEY")?;
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(app_private_key.as_bytes())?;

    let octocrab = Octocrab::builder().app(app_id, key).build()?;

    info!("Successfully registered as github app");

    Ok(octocrab)
}
