// Package index generator.
//
// Runs `nix search <flake-ref> --json ^` and transforms the raw
// output into a flat array of PackageEntry structs.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use tracing::{debug, info};

use super::super::types::PackageEntry;

/// Raw shape of each entry returned by `nix search --json`.
#[derive(serde::Deserialize)]
struct NixSearchEntry {
    pname: String,
    version: String,
    #[serde(default)]
    description: String,
}

/// Generate the package entries by shelling out to `nix search`.
///
/// Returns the parsed and sorted entries ready for insertion into the
/// SQLite database.
pub async fn generate_package_entries(flake_ref: &str, system: &str) -> Result<Vec<PackageEntry>> {
    let start = Instant::now();

    let raw_json = run_nix_search(flake_ref).await?;
    let entries = parse_and_transform(&raw_json, system)?;

    info!(
        event = "packages_entries_generated",
        entries = entries.len(),
        elapsed_ms = start.elapsed().as_millis() as u64,
        "generated package entries"
    );

    Ok(entries)
}

/// Run `nix search <flake_ref> --json ^` and return raw stdout.
async fn run_nix_search(flake_ref: &str) -> Result<Vec<u8>> {
    debug!(
        event = "nix_search_start",
        flake_ref = %flake_ref,
        "starting nix search"
    );

    let output = tokio::process::Command::new("nix")
        .args(["search", flake_ref, "--json", "^"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .context("failed to spawn `nix search`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`nix search {}` exited with {}: {}",
            flake_ref,
            output.status,
            stderr.chars().take(500).collect::<String>()
        );
    }

    Ok(output.stdout)
}

/// Parse the raw `nix search --json` output and strip the
/// `legacyPackages.{system}.` prefix from attribute paths.
fn parse_and_transform(raw: &[u8], system: &str) -> Result<Vec<PackageEntry>> {
    let map: HashMap<String, NixSearchEntry> =
        serde_json::from_slice(raw).context("failed to parse nix search JSON")?;

    let prefix = format!("legacyPackages.{system}.");
    let mut entries: Vec<PackageEntry> = map
        .into_iter()
        .map(|(full_attr, entry)| {
            let attr = full_attr
                .strip_prefix(&prefix)
                .unwrap_or(&full_attr)
                .to_string();
            PackageEntry {
                attr,
                pname: entry.pname,
                version: entry.version,
                description: entry.description,
                outputs: Vec::new(),
                main_program: None,
            }
        })
        .collect();

    // Sort for deterministic output.
    entries.sort_by(|a, b| a.attr.cmp(&b.attr));
    Ok(entries)
}

/// Enrich package entries with output names from existing drv data.
///
/// This is called after the initial entries are built, using data
/// already available in the evaluator's `NixEvalDrv.outputs` map.
pub fn enrich_with_outputs(
    entries: &mut [PackageEntry],
    output_map: &HashMap<String, Vec<String>>,
) {
    for entry in entries.iter_mut() {
        if let Some(outputs) = output_map.get(&entry.attr) {
            entry.outputs.clone_from(outputs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_strips_prefix_and_sorts() {
        let raw = serde_json::json!({
            "legacyPackages.x86_64-linux.hello": {
                "pname": "hello",
                "version": "2.12.1",
                "description": "A greeting"
            },
            "legacyPackages.x86_64-linux.bat": {
                "pname": "bat",
                "version": "0.24.0",
                "description": "cat clone"
            }
        });
        let bytes = serde_json::to_vec(&raw).unwrap();
        let entries = parse_and_transform(&bytes, "x86_64-linux").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].attr, "bat");
        assert_eq!(entries[1].attr, "hello");
        assert_eq!(entries[1].version, "2.12.1");
    }
}
