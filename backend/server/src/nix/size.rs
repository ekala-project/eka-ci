use std::collections::HashMap;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Information about a Nix store path from `nix path-info`
#[derive(Debug, Deserialize)]
struct PathInfo {
    path: String,
    #[serde(rename = "narSize")]
    nar_size: u64,
    #[serde(rename = "closureSize")]
    closure_size: u64,
}

/// Run `nix path-info --json` for the given paths and parse the result.
fn query_path_infos(output_paths: &[String], include_closure: bool) -> Result<Vec<PathInfo>> {
    let mut cmd = Command::new("nix");
    cmd.arg("path-info");
    if include_closure {
        cmd.arg("-S");
    }
    cmd.arg("--json").args(output_paths);

    let output = cmd.output().context("Failed to execute 'nix path-info'")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "nix path-info failed with status {}: {}",
            output.status,
            stderr
        );
    }

    serde_json::from_slice(&output.stdout).context("Failed to parse nix path-info JSON output")
}

/// Calculate per-path output (NAR) sizes for a list of Nix store paths.
///
/// Returns a map from store path → NAR size in bytes. Paths not reported by
/// `nix path-info` will be absent from the map.
pub fn get_output_sizes(output_paths: &[String]) -> Result<HashMap<String, u64>> {
    if output_paths.is_empty() {
        return Ok(HashMap::new());
    }

    let path_infos = query_path_infos(output_paths, false)?;
    Ok(path_infos
        .into_iter()
        .map(|info| (info.path, info.nar_size))
        .collect())
}

/// Calculate per-path closure sizes for a list of Nix store paths.
///
/// Returns a map from store path → closure size in bytes (path + runtime deps).
/// Paths not reported by `nix path-info` will be absent from the map.
pub fn get_closure_sizes(output_paths: &[String]) -> Result<HashMap<String, u64>> {
    if output_paths.is_empty() {
        return Ok(HashMap::new());
    }

    let path_infos = query_path_infos(output_paths, true)?;
    Ok(path_infos
        .into_iter()
        .map(|info| (info.path, info.closure_size))
        .collect())
}

/// Format bytes as human-readable size (e.g., "1.5 MB", "234 KB")
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(500), "500 bytes");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_size(1536 * 1024), "1.50 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
        assert_eq!(format_size(1536 * 1024 * 1024), "1.50 GB");
    }

    #[test]
    fn test_empty_paths() {
        let result = get_output_sizes(&[]);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());

        let result = get_closure_sizes(&[]);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
