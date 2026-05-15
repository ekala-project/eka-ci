use std::path::Path;

use anyhow::{Context, Result};
use ci_config::CIConfig;

/// Load checks configuration from .ekaci/config.json
pub async fn load_config(repo_path: &Path) -> Result<CIConfig> {
    let config_path = repo_path.join(".ekaci/config.json");

    let content = tokio::fs::read_to_string(&config_path)
        .await
        .with_context(|| format!("failed to read config file at {}", config_path.display()))?;

    let config = CIConfig::from_str(&content).context("failed to parse .ekaci/config.json")?;

    Ok(config)
}
