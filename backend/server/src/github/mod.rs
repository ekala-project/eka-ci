use anyhow::Context;
use octocrab::models::Installation;
use octocrab::{Octocrab, Page};
use thiserror::Error;
use tracing::info;

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

pub async fn register_app() -> Result<Page<Installation>, AppRegistrationError> {
    let app_id = std::env::var("GITHUB_APP_ID")
        .context("failed to locate $GITHUB_APP_ID")?
        .parse::<u64>()?
        .into();

    let app_private_key = std::env::var("GITHUB_APP_PRIVATE_KEY")
        .context("failed to locate $GITHUB_APP_PRIVATE_KEY")?;
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(app_private_key.as_bytes())?;

    let octocrab = Octocrab::builder().app(app_id, key).build()?;
    let installations = octocrab.apps().installations().send().await?;

    info!("Successfully registered as github app");

    Ok(installations)
}
