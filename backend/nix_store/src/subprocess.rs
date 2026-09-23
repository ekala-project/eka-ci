use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::store::{NixStore, PathInfo};

const QUICK_TIMEOUT: Duration = Duration::from_secs(30);

/// NixStore implementation that shells out to nix-store/nix CLI commands.
/// This is the fallback implementation and the one used for testing.
pub struct SubprocessNixStore;

impl SubprocessNixStore {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SubprocessNixStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl NixStore for SubprocessNixStore {
    async fn is_valid_path(&self, store_path: &str) -> Result<bool> {
        let output = tokio::time::timeout(
            QUICK_TIMEOUT,
            Command::new("nix-store")
                .args(["--check-validity", store_path])
                .output(),
        )
        .await
        .context("nix-store --check-validity timed out")?
        .context("failed to run nix-store --check-validity")?;

        Ok(output.status.success())
    }

    async fn query_references(&self, drv_path: &str) -> Result<Vec<String>> {
        let output = Command::new("nix-store")
            .args(["--query", "--references", drv_path])
            .output()
            .await?
            .stdout;
        let drv_str = String::from_utf8(output)?;

        let drvs = drv_str
            .lines()
            .filter(|x| x.ends_with(".drv"))
            .map(|x| x.to_string())
            .collect();

        Ok(drvs)
    }

    async fn query_requisites(&self, drv_path: &str) -> Result<Vec<String>> {
        let output = Command::new("nix-store")
            .args(["--query", "--requisites", drv_path])
            .output()
            .await?
            .stdout;
        let drv_str = String::from_utf8(output)?;

        let drvs = drv_str
            .lines()
            .filter(|x| x.ends_with(".drv"))
            .map(|x| x.to_string())
            .collect();

        Ok(drvs)
    }

    async fn query_derivation_output_map(&self, drv_path: &str) -> Result<HashMap<String, String>> {
        let output = Command::new("nix")
            .args(["derivation", "show", drv_path])
            .output()
            .await
            .context("Failed to execute nix derivation show")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("nix derivation show failed for {}: {}", drv_path, stderr);
        }

        let json_str = String::from_utf8(output.stdout)?;
        let parsed: serde_json::Value = serde_json::from_str(&json_str)?;

        let mut result = HashMap::new();
        // The output is { "/nix/store/...drv": { "outputs": { "out": { "path": "..." }, ... } } }
        if let Some(drv_info) = parsed.as_object().and_then(|m| m.values().next()) {
            if let Some(outputs) = drv_info.get("outputs").and_then(|o| o.as_object()) {
                for (name, info) in outputs {
                    if let Some(path) = info.get("path").and_then(|p| p.as_str()) {
                        result.insert(name.clone(), path.to_string());
                    }
                }
            }
        }

        // Fallback to nix-store --query --outputs if nix derivation show
        // didn't produce output paths
        if result.is_empty() {
            let fallback = Command::new("nix-store")
                .args(["--query", "--outputs", drv_path])
                .output()
                .await?;
            if fallback.status.success() {
                let paths = String::from_utf8(fallback.stdout)?;
                if let Some(first) = paths.lines().next() {
                    let trimmed = first.trim();
                    if !trimmed.is_empty() {
                        result.insert("out".to_string(), trimmed.to_string());
                    }
                }
            }
        }

        Ok(result)
    }

    async fn query_path_info(&self, store_path: &str) -> Result<PathInfo> {
        let output = tokio::time::timeout(
            QUICK_TIMEOUT,
            Command::new("nix")
                .args(["path-info", "--json", store_path])
                .output(),
        )
        .await
        .context("nix path-info timed out")?
        .context("failed to run nix path-info")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("nix path-info failed for {}: {}", store_path, stderr);
        }

        let json_str = String::from_utf8(output.stdout)?;
        let parsed: serde_json::Value = serde_json::from_str(&json_str)?;

        // nix path-info --json returns an array with one element
        let info = parsed
            .as_array()
            .and_then(|a| a.first())
            .context("empty path-info response")?;

        let nar_size = info.get("narSize").and_then(|v| v.as_u64()).unwrap_or(0);

        let references = info
            .get("references")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(PathInfo {
            nar_size,
            references,
        })
    }

    async fn store_ping(&self) -> Result<()> {
        let output = tokio::time::timeout(
            QUICK_TIMEOUT,
            Command::new("nix").args(["store", "ping"]).output(),
        )
        .await
        .context("nix store ping timed out")?
        .context("failed to run nix store ping")?;

        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("nix store ping failed: {}", stderr.trim())
        }
    }
}
