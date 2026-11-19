//! Output (NAR) and closure size measurement via `nix path-info`.
//!
//! A single `nix path-info --recursive --json` call returns the NAR size and
//! the references of every path in the combined runtime closure of the
//! requested outputs. From that we derive:
//!
//! - the NAR size of each output,
//! - the closure size of each output (the output plus its runtime deps),
//! - the total output size of the derivation (sum of its outputs), and
//! - the total closure size of the derivation, counting each path once even when
//!   several outputs share it.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use serde::Deserialize;
use tokio::process::Command;

/// Sizes of one output of a derivation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputSize {
    /// NAR size of the output path itself, in bytes.
    pub nar_size: u64,
    /// NAR size of the output and everything it references at runtime.
    pub closure_size: u64,
    /// Number of store paths in the output's runtime closure, itself included.
    pub closure_paths: usize,
}

/// Sizes of all requested outputs of a derivation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputSizeReport {
    /// Per-output sizes, keyed by the full output store path.
    pub outputs: HashMap<String, OutputSize>,
    /// Sum of the NAR sizes of the requested outputs.
    pub total_nar_size: u64,
    /// NAR size of the union of the outputs' closures (shared paths counted once).
    pub total_closure_size: u64,
}

/// One entry of `nix path-info --json`. Unknown fields are ignored.
#[derive(Debug, Deserialize)]
struct PathInfoEntry {
    #[serde(rename = "narSize")]
    nar_size: u64,
    #[serde(default)]
    references: Vec<String>,
}

/// Legacy (Nix < 2.19) array entry, which carries its own path.
#[derive(Debug, Deserialize)]
struct LegacyPathInfoEntry {
    path: String,
    #[serde(rename = "narSize", default)]
    nar_size: Option<u64>,
    #[serde(default)]
    references: Vec<String>,
}

/// The shapes `nix path-info --json` has produced over time:
/// - format 2 (Nix 2.33+ with `--json-format 2`): `{"info": {basename: entry}, ...}`
/// - format 1 (Nix 2.19+): `{"/nix/store/...": entry}`
/// - legacy (older Nix): `[{"path": "/nix/store/...", ...}]`
///
/// Invalid paths appear as `null` (formats 1 and 2) or without `narSize` (legacy).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PathInfoJson {
    V2 {
        info: HashMap<String, Option<PathInfoEntry>>,
    },
    V1(HashMap<String, Option<PathInfoEntry>>),
    Legacy(Vec<LegacyPathInfoEntry>),
}

/// Strip the store directory, so paths from every JSON format compare equal.
fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Parse `nix path-info --json` output into basename → (narSize, reference basenames).
/// Paths reported as invalid are left out.
fn parse_path_infos(json: &[u8]) -> Result<HashMap<String, (u64, Vec<String>)>> {
    let parsed: PathInfoJson =
        serde_json::from_slice(json).context("Failed to parse nix path-info JSON output")?;

    let normalize = |path: &str, nar_size: u64, refs: Vec<String>| {
        let refs = refs.iter().map(|r| base_name(r).to_string()).collect();
        (base_name(path).to_string(), (nar_size, refs))
    };

    let infos = match parsed {
        PathInfoJson::V2 { info: map } | PathInfoJson::V1(map) => map
            .into_iter()
            .filter_map(|(path, entry)| entry.map(|e| normalize(&path, e.nar_size, e.references)))
            .collect(),
        PathInfoJson::Legacy(entries) => entries
            .into_iter()
            .filter_map(|e| {
                let nar_size = e.nar_size?;
                Some(normalize(&e.path, nar_size, e.references))
            })
            .collect(),
    };
    Ok(infos)
}

/// Collect every path reachable from `root` through the reference graph.
fn closure_of<'a>(
    root: &'a str,
    infos: &'a HashMap<String, (u64, Vec<String>)>,
    seen: &mut HashSet<&'a str>,
) -> Result<()> {
    let mut stack = vec![root];
    while let Some(path) = stack.pop() {
        if !seen.insert(path) {
            continue;
        }
        let (_, refs) = infos
            .get(path)
            .with_context(|| format!("nix path-info did not report closure member {path}"))?;
        stack.extend(refs.iter().map(String::as_str));
    }
    Ok(())
}

fn sum_nar(paths: &HashSet<&str>, infos: &HashMap<String, (u64, Vec<String>)>) -> u64 {
    paths
        .iter()
        .filter_map(|p| infos.get(*p))
        .map(|i| i.0)
        .sum()
}

/// Build the size report for `output_paths` from parsed path-info data.
fn compute_report(
    output_paths: &[String],
    infos: &HashMap<String, (u64, Vec<String>)>,
) -> Result<OutputSizeReport> {
    let mut report = OutputSizeReport::default();
    let mut union: HashSet<&str> = HashSet::new();

    for output in output_paths {
        let name = base_name(output);
        let (nar_size, _) = infos
            .get(name)
            .with_context(|| format!("{output} is not a valid path in the queried store"))?;

        let mut closure = HashSet::new();
        closure_of(name, infos, &mut closure)?;
        let closure_size = sum_nar(&closure, infos);
        let closure_paths = closure.len();
        union.extend(closure);

        report.total_nar_size += nar_size;
        report.outputs.insert(
            output.clone(),
            OutputSize {
                nar_size: *nar_size,
                closure_size,
                closure_paths,
            },
        );
    }

    report.total_closure_size = sum_nar(&union, infos);
    Ok(report)
}

/// Measure the output and closure sizes of `output_paths`.
///
/// `store` selects the store to query (`--store`); `None` means the default
/// (local) store. Querying a binary cache URL works without downloading NARs,
/// because NAR sizes and references come from the `.narinfo` files.
///
/// Fails if any output is not valid in the queried store.
pub async fn measure_outputs(
    output_paths: &[String],
    store: Option<&str>,
) -> Result<OutputSizeReport> {
    if output_paths.is_empty() {
        return Ok(OutputSizeReport::default());
    }

    let mut cmd = Command::new("nix");
    cmd.kill_on_drop(true);
    cmd.args(["path-info", "--recursive", "--json"]);
    if let Some(store) = store {
        cmd.args(["--store", store]);
    }
    cmd.args(output_paths);

    let output = cmd
        .output()
        .await
        .context("Failed to execute 'nix path-info'")?;

    // Invalid paths make nix exit non-zero but may still print JSON; only
    // trust the result when every output is present.
    let infos = match parse_path_infos(&output.stdout) {
        Ok(infos) => infos,
        Err(_) if !output.status.success() => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!(
                "nix path-info failed with {}: {}",
                output.status,
                stderr.trim()
            );
        },
        Err(e) => return Err(e),
    };
    compute_report(output_paths, &infos)
}

/// The substituters the local Nix is configured with (`nix config show substituters`).
pub async fn configured_substituters() -> Result<Vec<String>> {
    let output = Command::new("nix")
        .kill_on_drop(true)
        .args(["config", "show", "substituters"])
        .output()
        .await
        .context("Failed to execute 'nix config show substituters'")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix config show failed: {}", stderr.trim());
    }

    let mut seen = HashSet::new();
    Ok(String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| seen.insert(s.clone()))
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

    #[tokio::test]
    async fn test_empty_paths() {
        let report = measure_outputs(&[], None).await.unwrap();
        assert_eq!(report, OutputSizeReport::default());
    }

    /// out → lib → glibc, dev → out. glibc is shared by both closures.
    const V1: &str = r#"{
        "/nix/store/aaa-pkg": {"narSize": 100, "references": ["/nix/store/ccc-lib", "/nix/store/aaa-pkg"], "ultimate": false},
        "/nix/store/bbb-pkg-dev": {"narSize": 10, "references": ["/nix/store/aaa-pkg"]},
        "/nix/store/ccc-lib": {"narSize": 1000, "references": ["/nix/store/ddd-glibc"]},
        "/nix/store/ddd-glibc": {"narSize": 5000, "references": []}
    }"#;

    const V2: &str = r#"{"info": {
        "aaa-pkg": {"narSize": 100, "references": ["ccc-lib", "aaa-pkg"], "version": 2},
        "bbb-pkg-dev": {"narSize": 10, "references": ["aaa-pkg"]},
        "ccc-lib": {"narSize": 1000, "references": ["ddd-glibc"]},
        "ddd-glibc": {"narSize": 5000, "references": []}
    }, "storeDir": "/nix/store", "version": 2}"#;

    const LEGACY: &str = r#"[
        {"path": "/nix/store/aaa-pkg", "narSize": 100, "references": ["/nix/store/ccc-lib"]},
        {"path": "/nix/store/bbb-pkg-dev", "narSize": 10, "references": ["/nix/store/aaa-pkg"]},
        {"path": "/nix/store/ccc-lib", "narSize": 1000, "references": ["/nix/store/ddd-glibc"]},
        {"path": "/nix/store/ddd-glibc", "narSize": 5000, "references": []}
    ]"#;

    fn outputs() -> Vec<String> {
        vec![
            "/nix/store/aaa-pkg".to_string(),
            "/nix/store/bbb-pkg-dev".to_string(),
        ]
    }

    fn assert_expected(report: &OutputSizeReport) {
        let out = report.outputs["/nix/store/aaa-pkg"];
        let dev = report.outputs["/nix/store/bbb-pkg-dev"];
        assert_eq!(out.nar_size, 100);
        assert_eq!(out.closure_size, 6100);
        assert_eq!(out.closure_paths, 3);
        assert_eq!(dev.nar_size, 10);
        assert_eq!(dev.closure_size, 6110);
        assert_eq!(dev.closure_paths, 4);
        assert_eq!(report.total_nar_size, 110);
        // Shared paths are counted once, not 6100 + 6110.
        assert_eq!(report.total_closure_size, 6110);
    }

    #[test]
    fn report_from_every_json_format() {
        for json in [V1, V2, LEGACY] {
            let infos = parse_path_infos(json.as_bytes()).unwrap();
            assert_expected(&compute_report(&outputs(), &infos).unwrap());
        }
    }

    #[test]
    fn invalid_output_is_an_error() {
        let json = r#"{"/nix/store/aaa-pkg": null}"#;
        let infos = parse_path_infos(json.as_bytes()).unwrap();
        let err = compute_report(&["/nix/store/aaa-pkg".to_string()], &infos).unwrap_err();
        assert!(err.to_string().contains("not a valid path"), "{err}");
    }

    #[test]
    fn missing_closure_member_is_an_error() {
        let json = r#"{"/nix/store/aaa-pkg": {"narSize": 1, "references": ["/nix/store/zzz"]}}"#;
        let infos = parse_path_infos(json.as_bytes()).unwrap();
        assert!(compute_report(&["/nix/store/aaa-pkg".to_string()], &infos).is_err());
    }
}
