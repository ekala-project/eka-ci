use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;
use tracing::debug;

use crate::types::derivation_show;

/// How long to wait for `nix-store --realise --dry-run` to complete before
/// giving up. Substituter HTTP queries are the dominant cost; 30s comfortably
/// covers a slow but reachable cache while still failing fast on a wedged one.
const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Retrieve the requisites of a drv. This is a global list of all direct
/// and transitive drvs
pub async fn drv_requisites(drv_path: &str) -> Result<Vec<String>> {
    let output = Command::new("nix-store")
        .args(["--query", "--requisites", drv_path])
        .output()
        .await?
        .stdout;
    let drv_str = String::from_utf8(output)?;

    let drvs = drv_str
        .lines()
        // drv requisites can include "inputSrcs" which are not inputDrvs
        // but rather files which were added to the nix store through
        // path literals or `nix-store --add`
        .filter(|x| x.ends_with(".drv"))
        .map(|x| x.to_string())
        .collect::<Vec<String>>();

    Ok(drvs)
}

/// Retrieve the direct dependencies of a drv
pub async fn drv_references(drv_path: &str) -> Result<Vec<String>> {
    let output = Command::new("nix-store")
        .args(["--query", "--references", drv_path])
        .output()
        .await?
        .stdout;
    let drv_str = String::from_utf8(output)?;

    let drvs = drv_str
        .lines()
        // drv references can include "inputSrcs" which are not inputDrvs
        // but rather files which were added to the nix store through
        // path literals or `nix-store --add`
        .filter(|x| x.ends_with(".drv"))
        .map(|x| x.to_string())
        .collect::<Vec<String>>();

    Ok(drvs)
}

/// Retrieve the runtime references of an output path (retained dependencies)
///
/// This queries what store paths are actually referenced by a built output,
/// which represents the true runtime dependencies (what ends up in the closure).
///
/// # Arguments
/// * `output_path` - Full store path to a built output (e.g., `/nix/store/hash-name`)
///
/// # Returns
/// Vector of full store paths that are runtime dependencies
pub async fn output_references(output_path: &str) -> Result<Vec<String>> {
    let output = Command::new("nix-store")
        .args(["--query", "--references", output_path])
        .output()
        .await?
        .stdout;
    let refs_str = String::from_utf8(output)?;

    let refs = refs_str
        .lines()
        .filter(|x| !x.is_empty())
        .map(|x| x.trim().to_string())
        .collect::<Vec<String>>();

    Ok(refs)
}

/// Get the outputs of a derivation with their names
///
/// Uses `nix derivation show` to get structured information about a derivation's outputs.
///
/// # Arguments
/// * `drv_path` - Path to the .drv file
///
/// # Returns
/// HashMap mapping output names (e.g., "out", "dev", "doc") to their store paths
pub async fn get_drv_outputs(drv_path: &str) -> Result<HashMap<String, String>> {
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
    let drv_output: derivation_show::DrvOutput =
        serde_json::from_str(&json_str).context("Failed to parse nix derivation show output")?;

    // The output is a map with the drv path as key, get the first (and only) value
    let drv_info = drv_output
        .drvs
        .into_values()
        .next()
        .context("No derivation info found in output")?;

    // Extract outputs map
    let outputs_map: HashMap<String, String> = if let Some(outputs) = drv_info.outputs {
        outputs
            .into_iter()
            .map(|(name, info)| (name, info.path))
            .collect()
    } else {
        // Fallback: if no outputs field, assume single "out" output
        // Try using nix-store --query --outputs as fallback
        let fallback_output = Command::new("nix-store")
            .args(["--query", "--outputs", drv_path])
            .output()
            .await?;

        if fallback_output.status.success() {
            let paths = String::from_utf8(fallback_output.stdout)?;
            let first_path = paths.lines().next().unwrap_or("").trim();
            if !first_path.is_empty() {
                let mut map = HashMap::new();
                map.insert("out".to_string(), first_path.to_string());
                map
            } else {
                HashMap::new()
            }
        } else {
            HashMap::new()
        }
    };

    Ok(outputs_map)
}

/// Run `nix-store --realise --dry-run <drv>` to check if a derivation is cached.
///
/// Returns true if the derivation's output is available (either in the local store
/// or from a substituter), false if it would need to be built.
pub async fn is_drv_cached(drv_path: &str) -> Result<bool> {
    debug!("Checking if {} is cached", drv_path);

    let output = tokio::time::timeout(
        DRY_RUN_TIMEOUT,
        Command::new("nix-store")
            .args(["--realise", "--dry-run", drv_path])
            .output(),
    )
    .await
    .with_context(|| format!("nix-store --realise --dry-run timed out for {}", drv_path))?
    .with_context(|| {
        format!(
            "failed to spawn nix-store --realise --dry-run for {}",
            drv_path
        )
    })?;

    if !output.status.success() {
        // If the command failed, assume not cached
        return Ok(false);
    }

    // Nix writes the build/fetch plan to stderr
    let stderr = String::from_utf8_lossy(&output.stderr);

    // If "will be built" appears in the output, it's not cached
    let needs_build = stderr.contains("will be built")
        || stderr.contains("derivation(s) will be built")
        || stderr.contains("don't know how to build");

    Ok(!needs_build)
}
