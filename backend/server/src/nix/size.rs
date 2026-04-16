use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Information about a Nix store path from `nix path-info`
#[derive(Debug, Deserialize)]
struct PathInfo {
    #[allow(dead_code)]
    path: String,
    #[serde(rename = "narSize")]
    nar_size: u64,
    #[serde(rename = "closureSize")]
    closure_size: u64,
}

/// Calculate the total output size for a list of Nix store paths.
///
/// This uses `nix path-info --json` to get the NAR (Nix Archive) size for each output path.
/// The NAR size represents the actual disk space used by the store path.
///
/// # Arguments
/// * `output_paths` - List of Nix store paths to measure (e.g., ["/nix/store/abc-foo"])
///
/// # Returns
/// * `Ok(u64)` - Total size in bytes
/// * `Err` - If nix command fails or paths don't exist
///
/// # Example
/// ```no_run
/// use eka_ci_server::nix::size::get_output_size;
///
/// let paths = vec!["/nix/store/abc-python".to_string()];
/// let size = get_output_size(&paths)?;
/// println!("Total output size: {} MB", size / 1024 / 1024);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn get_output_size(output_paths: &[String]) -> Result<u64> {
    if output_paths.is_empty() {
        return Ok(0);
    }

    // Use nix path-info to get size information in JSON format
    let output = Command::new("nix")
        .arg("path-info")
        .arg("--json")
        .args(output_paths)
        .output()
        .context("Failed to execute 'nix path-info'")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "nix path-info failed with status {}: {}",
            output.status,
            stderr
        );
    }

    // Parse JSON response
    let path_infos: Vec<PathInfo> = serde_json::from_slice(&output.stdout)
        .context("Failed to parse nix path-info JSON output")?;

    // Sum up all NAR sizes
    let total_size: u64 = path_infos.iter().map(|info| info.nar_size).sum();

    Ok(total_size)
}

/// Calculate the total closure size for a list of Nix store paths.
///
/// This uses `nix path-info -S --json` to get the closure size for each output path.
/// The closure size includes the path itself plus all of its runtime dependencies.
///
/// # Arguments
/// * `output_paths` - List of Nix store paths to measure (e.g., ["/nix/store/abc-foo"])
///
/// # Returns
/// * `Ok(u64)` - Total closure size in bytes (sum of all paths' closures)
/// * `Err` - If nix command fails or paths don't exist
///
/// # Example
/// ```no_run
/// use eka_ci_server::nix::size::get_closure_size;
///
/// let paths = vec!["/nix/store/abc-python".to_string()];
/// let size = get_closure_size(&paths)?;
/// println!("Total closure size: {} MB", size / 1024 / 1024);
/// # Ok::<(), anyhow::Error>(())
/// ```
pub fn get_closure_size(output_paths: &[String]) -> Result<u64> {
    if output_paths.is_empty() {
        return Ok(0);
    }

    // Use nix path-info with -S flag to get closure size information in JSON format
    let output = Command::new("nix")
        .arg("path-info")
        .arg("-S")
        .arg("--json")
        .args(output_paths)
        .output()
        .context("Failed to execute 'nix path-info -S'")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "nix path-info -S failed with status {}: {}",
            output.status,
            stderr
        );
    }

    // Parse JSON response
    let path_infos: Vec<PathInfo> = serde_json::from_slice(&output.stdout)
        .context("Failed to parse nix path-info JSON output")?;

    // Sum up all closure sizes
    let total_size: u64 = path_infos.iter().map(|info| info.closure_size).sum();

    Ok(total_size)
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
        let result = get_output_size(&[]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }
}
