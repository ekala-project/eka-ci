use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;
use tracing::debug;

use crate::types::derivation_show;

/// How long to wait for `nix-store --realise --dry-run` to complete before
/// giving up. Substituter HTTP queries are the dominant cost; 30s comfortably
/// covers a slow but reachable cache while still failing fast on a wedged one.
const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Result of a `nix-store --realise --dry-run` invocation against a derivation.
///
/// Nix prints two relevant sections to stderr:
/// * `(this|these N) derivation(s) will be built:` followed by indented `.drv` paths that are NOT
///   available locally and CANNOT be substituted from any configured binary cache — i.e. they would
///   actually need to run.
/// * `(this|these N) path(s) will be fetched (...)` followed by indented store output paths that
///   are not in the local store but ARE available from a binary cache.
///
/// Anything not mentioned in either section is already valid in the local store.
///
/// We treat a derivation as "cached" iff its `.drv` path does not appear in
/// `will_build` — meaning it is either already built locally OR pullable from
/// a substituter, both of which mean we don't need to run a real build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunReport {
    /// Full store paths (`/nix/store/...drv`) that nix says still need building.
    pub will_build: HashSet<String>,
    /// Full store paths (`/nix/store/...`) that nix says will be substituted.
    pub will_fetch: HashSet<String>,
}

impl DryRunReport {
    /// Parse the stderr emitted by `nix-store --realise --dry-run`.
    ///
    /// The format is fairly stable across modern Nix versions; we tolerate
    /// minor variations (singular vs. plural section headers, optional
    /// download-size annotations, leading whitespace differences).
    pub fn parse(stderr: &str) -> Self {
        #[derive(Clone, Copy)]
        enum Section {
            Build,
            Fetch,
        }

        let mut report = DryRunReport::default();
        let mut current: Option<Section> = None;

        for raw_line in stderr.lines() {
            let trimmed = raw_line.trim_end();
            let lower = trimmed.trim_start().to_ascii_lowercase();

            if !raw_line.starts_with(char::is_whitespace) {
                if lower.contains("will be built") || lower.starts_with("don't know how to build") {
                    current = Some(Section::Build);
                    continue;
                }
                if lower.contains("will be fetched") {
                    current = Some(Section::Fetch);
                    continue;
                }
                if !trimmed.is_empty() {
                    current = None;
                }
                continue;
            }

            let path = trimmed.trim();
            if path.is_empty() {
                continue;
            }
            if !path.starts_with('/') {
                continue;
            }
            match current {
                Some(Section::Build) => {
                    report.will_build.insert(path.to_string());
                },
                Some(Section::Fetch) => {
                    report.will_fetch.insert(path.to_string());
                },
                None => {},
            }
        }

        report
    }

    /// True iff `drv_store_path` does not appear in `will_build` — i.e. its
    /// output is either already in the local store or pullable from a substituter.
    pub fn is_cached_path(&self, drv_store_path: &str) -> bool {
        !self.will_build.contains(drv_store_path)
    }
}

/// Run `nix-store --realise --dry-run <drv>` and parse the result.
///
/// Returns `Err` for any I/O / process failure or non-zero exit so callers can
/// safely fall back to a real build (substitution checks are an optimization,
/// never a correctness requirement).
pub async fn dry_run_realise(drv_store_path: &str) -> Result<DryRunReport> {
    let output = tokio::time::timeout(
        DRY_RUN_TIMEOUT,
        Command::new("nix-store")
            .args(["--realise", "--dry-run", drv_store_path])
            .output(),
    )
    .await
    .with_context(|| format!("nix-store --realise --dry-run timed out for {drv_store_path}"))?
    .with_context(|| {
        format!("failed to spawn nix-store --realise --dry-run for {drv_store_path}")
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "nix-store --realise --dry-run failed for {} (exit {:?}): {}",
            drv_store_path,
            output.status.code(),
            stderr.trim()
        );
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(DryRunReport::parse(&stderr))
}

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
        serde_json::from_str(&json_str).with_context(|| {
            let snippet = if json_str.len() > 200 {
                &json_str[..200]
            } else {
                &json_str
            };
            format!(
                "Failed to parse `nix derivation show` output for {}: {}...",
                drv_path, snippet
            )
        })?;

    // The output is a map with the drv path as key, get the first (and only) value
    let drv_info = drv_output
        .into_drvs()
        .into_values()
        .next()
        .context("No derivation info found in output")?;

    // Extract outputs map
    let outputs_map: HashMap<String, String> = if let Some(outputs) = drv_info.outputs {
        outputs
            .into_iter()
            .filter_map(|(name, info)| info.path.map(|p| (name, p)))
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

    match dry_run_realise(drv_path).await {
        Ok(report) => Ok(report.is_cached_path(drv_path)),
        Err(_) => Ok(false),
    }
}
