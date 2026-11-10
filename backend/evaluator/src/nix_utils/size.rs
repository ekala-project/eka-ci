use std::collections::HashMap;
use std::process::Command;

use anyhow::{Context, Result};
use serde::Deserialize;

/// Information about a Nix store path from `nix path-info`
///
/// The JSON output is an object like:
/// ```json
/// {
///   "/nix/store/...": {
///     "narSize": 123,
///     "closureSize": 456,  // only with -S flag
///     ...
///   }
/// }
/// ```
#[derive(Debug, Deserialize)]
struct PathInfoData {
    #[serde(rename = "narSize")]
    nar_size: u64,
    #[serde(rename = "closureSize", default)]
    closure_size: u64,
}

/// Run `nix path-info --json` with `flags` for the given paths and parse the
/// result.
fn query_path_infos(
    output_paths: &[String],
    flags: &[&str],
) -> Result<HashMap<String, PathInfoData>> {
    let mut cmd = Command::new("nix");
    cmd.arg("path-info")
        .args(flags)
        .arg("--json")
        .args(output_paths);

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

    let path_infos = query_path_infos(output_paths, &[])?;
    Ok(path_infos
        .into_iter()
        .map(|(path, info)| (path, info.nar_size))
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

    let path_infos = query_path_infos(output_paths, &["-S"])?;
    Ok(path_infos
        .into_iter()
        .map(|(path, info)| (path, info.closure_size))
        .collect())
}

/// Calculate the size of the combined closure of several store paths.
///
/// Unlike summing [`get_closure_sizes`], runtime dependencies shared by
/// several paths (e.g. the outputs of one derivation) are counted once.
pub fn get_combined_closure_size(output_paths: &[String]) -> Result<u64> {
    if output_paths.is_empty() {
        return Ok(0);
    }

    let path_infos = query_path_infos(output_paths, &["--recursive"])?;
    Ok(sum_nar_sizes(&path_infos))
}

/// Total NAR size of a set of path infos, each path counted once.
fn sum_nar_sizes(path_infos: &HashMap<String, PathInfoData>) -> u64 {
    path_infos.values().map(|info| info.nar_size).sum()
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

        assert_eq!(get_combined_closure_size(&[]).unwrap(), 0);
    }

    #[test]
    fn combined_closure_counts_shared_paths_once() {
        // `nix path-info --recursive --json out dev`: both outputs depend on
        // glibc, which appears once in the result.
        let json = r#"{
            "/nix/store/aaa-hello": { "narSize": 100 },
            "/nix/store/bbb-hello-dev": { "narSize": 10 },
            "/nix/store/ccc-glibc": { "narSize": 1000 }
        }"#;
        let infos: HashMap<String, PathInfoData> = serde_json::from_str(json).unwrap();
        assert_eq!(sum_nar_sizes(&infos), 1110);
    }

    #[test]
    fn test_parse_path_info_json() {
        // Test parsing real-world nix path-info JSON output
        let json_without_closure = r#"{
            "/nix/store/test.drv": {
                "ca": "text:sha256:abc123",
                "deriver": null,
                "narHash": "sha256-xyz",
                "narSize": 3264,
                "references": [],
                "registrationTime": 1777484331,
                "signatures": [],
                "ultimate": false
            }
        }"#;

        let result: Result<std::collections::HashMap<String, PathInfoData>, _> =
            serde_json::from_str(json_without_closure);
        assert!(result.is_ok(), "Should parse JSON without closureSize");
        let map = result.unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map.get("/nix/store/test.drv").unwrap().nar_size, 3264);
        assert_eq!(map.get("/nix/store/test.drv").unwrap().closure_size, 0); // default value

        let json_with_closure = r#"{
            "/nix/store/test.drv": {
                "ca": "text:sha256:abc123",
                "closureSize": 2294600,
                "deriver": null,
                "narHash": "sha256-xyz",
                "narSize": 3264,
                "references": [],
                "registrationTime": 1777484331,
                "signatures": [],
                "ultimate": false
            }
        }"#;

        let result: Result<std::collections::HashMap<String, PathInfoData>, _> =
            serde_json::from_str(json_with_closure);
        assert!(result.is_ok(), "Should parse JSON with closureSize");
        let map = result.unwrap();
        assert_eq!(map.get("/nix/store/test.drv").unwrap().nar_size, 3264);
        assert_eq!(
            map.get("/nix/store/test.drv").unwrap().closure_size,
            2294600
        );
    }
}
