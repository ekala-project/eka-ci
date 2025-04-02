use octocrab::{Octocrab, Page};
use octocrab::models::Installation;
use crate::types::GHPullInfo;
use crate::error;
use log::info;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::blocking::Response;

const GH_ACCEPT_VALUE: HeaderValue = HeaderValue::from_static("application/vnd.github+json");
const GH_API_NAME: HeaderName = HeaderName::from_static("x-github-api-version");
const GH_API_VALUE: HeaderValue = HeaderValue::from_static("2022-11-28");
const EKA_USER_AGENT: HeaderValue = HeaderValue::from_static("reqwest eka-ci");

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

fn github_headers() -> HeaderMap {
    use reqwest::header as h;

    let auth_token = std::env::var("GITHUB_TOKEN");
    let mut headers = HeaderMap::new();
    headers.insert(h::ACCEPT, GH_ACCEPT_VALUE);
    headers.insert(GH_API_NAME, GH_API_VALUE);
    headers.insert(h::USER_AGENT, EKA_USER_AGENT);
    if let Ok(token) = auth_token {
        headers.insert(h::AUTHORIZATION, h::HeaderValue::from_str(&token).expect("Invalid github token"));
    }
    headers
}

pub(crate) async fn github_pull(domain: &str, owner: &str, repo: &str, pull: u32) -> error::Result<GHPullInfo> {
    let repo_url = format!("https://api.{}/repos/{}/{}/pulls/{}", &domain, &owner, &repo, &pull);

    log::debug!("building client");
    let client = reqwest::Client::builder()
        .default_headers(github_headers())
        .build()?;
    log::debug!("Attempting to pull info, {}", &repo_url);
    let text = client.get(&repo_url)
        .send().await?
        .text().await?;

    log::debug!("Got text, {}", &text);
    let json: GHPullInfo = client.get(&repo_url)
        .send().await?
        .json::<GHPullInfo>().await?;
    Ok(json)
}
