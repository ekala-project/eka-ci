use octocrab::{Octocrab, Page};
use octocrab::models::Installation;
use octocrab;
use jsonwebtoken;
use crate::error;
use log::info;

pub async fn register_app() -> error::Result<Page<Installation>> {
    let app_id = std::env::var("GITHUB_APP_ID")
        .map(|x| x.parse::<u64>())??
        .into();
    let app_private_key = std::env::var("GITHUB_APP_PRIVATE_KEY")?;
    let key = jsonwebtoken::EncodingKey::from_rsa_pem(app_private_key.as_bytes()).expect("Failed to create json webtoken");

    let octocrab = Octocrab::builder().app(app_id, key).build()?;
    let installations = octocrab.apps().installations().send().await?;

    info!("Successfully registered as github app");

    Ok(installations)
}
