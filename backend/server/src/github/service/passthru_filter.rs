//! Filter changed packages to only those directly modified in a PR.
//!
//! Uses `meta.position` (package source file location) cross-referenced
//! with `git diff --name-only` to distinguish packages whose definition
//! was edited from packages that merely got rebuilt due to a dependency
//! change.

use std::collections::HashSet;

use anyhow::{Context, Result};
use sqlx::{Pool, Sqlite};
use tokio::process::Command;
use tracing::{debug, warn};

use crate::db::github::NewOrChangedJob;
use crate::db::model::DrvId;

/// Result of filtering changed jobs to directly-modified packages.
pub(crate) struct FilteredAttrs {
    /// Attr names of packages whose source file was in the git diff.
    pub directly_changed: Vec<String>,
}

/// Query `meta_position` for a set of drv_paths.
async fn get_meta_positions(
    drv_paths: &[&DrvId],
    pool: &Pool<Sqlite>,
) -> Result<Vec<(String, Option<String>)>> {
    // SQLite doesn't support array binds, so batch with IN clause.
    // Chunk to stay under the 999 variable limit.
    let mut results = Vec::new();

    for chunk in drv_paths.chunks(200) {
        let placeholders: String = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let query = format!(
            "SELECT drv_path, meta_position FROM Drv WHERE drv_path IN ({})",
            placeholders
        );

        let mut q = sqlx::query_as::<_, (String, Option<String>)>(&query);
        for drv_id in chunk {
            q = q.bind(drv_id.to_string());
        }
        let rows = q.fetch_all(pool).await?;
        results.extend(rows);
    }

    Ok(results)
}

/// Run `git diff --name-only` between two commits in the given repo dir.
async fn git_diff_changed_files(
    repo_dir: &str,
    base_sha: &str,
    head_sha: &str,
) -> Result<HashSet<String>> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Command::new("git")
            .current_dir(repo_dir)
            .args(["diff", "--name-only", base_sha, head_sha])
            .output(),
    )
    .await
    .context("git diff --name-only timed out")?
    .context("failed to execute git diff --name-only")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git diff --name-only failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: HashSet<String> = stdout.lines().map(|l| l.trim().to_string()).collect();
    debug!("git diff: {} files changed", files.len());
    Ok(files)
}

/// Extract the file path from a `meta.position` value.
///
/// `meta.position` has the format `<path>:<line>`, e.g.:
/// - `/nix/store/...-source/pkgs/foo/default.nix:34`
/// - `pkgs/foo/default.nix:34`
///
/// We strip the `:<line>` suffix and any nix store prefix to get a
/// repo-relative path that can be matched against `git diff` output.
fn position_to_repo_relative_path(position: &str) -> Option<String> {
    // Strip :<line> suffix
    let path = position
        .rsplit_once(':')
        .map(|(p, _)| p)
        .unwrap_or(position);

    if path.is_empty() {
        return None;
    }

    // If it starts with /nix/store, strip to find the repo-relative portion.
    // The pattern is `/nix/store/<hash>-<name>/rest/of/path` — the repo
    // content starts after the first directory inside the store path.
    if let Some(after_store) = path.strip_prefix("/nix/store/") {
        return after_store
            .split_once('/')
            .map(|(_, rest)| rest.to_string());
    }

    Some(path.to_string())
}

/// Filter changed jobs to only those whose source file was directly
/// modified in this PR.
///
/// Returns attr names that should have their `passthru.tests` evaluated.
pub(crate) async fn filter_directly_changed(
    changed_jobs: &[NewOrChangedJob],
    base_sha: &str,
    head_sha: &str,
    repo_dir: &str,
    pool: &Pool<Sqlite>,
) -> Result<FilteredAttrs> {
    if changed_jobs.is_empty() {
        return Ok(FilteredAttrs {
            directly_changed: Vec::new(),
        });
    }

    // Get changed files from git
    let changed_files = git_diff_changed_files(repo_dir, base_sha, head_sha).await?;

    if changed_files.is_empty() {
        return Ok(FilteredAttrs {
            directly_changed: Vec::new(),
        });
    }

    // Get meta_positions for all changed drv_paths
    let drv_paths: Vec<&DrvId> = changed_jobs.iter().map(|j| &j.drv_path).collect();
    let positions = get_meta_positions(&drv_paths, pool).await?;

    // Build a lookup from drv_path -> meta_position
    let position_map: std::collections::HashMap<String, Option<String>> =
        positions.into_iter().collect();

    // Filter: keep attrs whose meta.position file is in the changed set
    let mut directly_changed = Vec::new();
    for job in changed_jobs {
        let drv_str = job.drv_path.to_string();
        let meta_pos = position_map.get(&drv_str).and_then(|p| p.as_deref());

        if let Some(pos) = meta_pos {
            if let Some(rel_path) = position_to_repo_relative_path(pos) {
                if changed_files.contains(&rel_path) {
                    debug!(
                        "passthru.tests: {} directly changed (position {} matches git diff)",
                        job.name, rel_path
                    );
                    directly_changed.push(job.name.clone());
                    continue;
                }
            }
        } else {
            // No meta.position — log and skip
            warn!(
                "passthru.tests: {} has no meta.position, skipping",
                job.name
            );
        }
    }

    debug!(
        "passthru.tests filter: {} of {} changed packages are directly modified",
        directly_changed.len(),
        changed_jobs.len()
    );

    Ok(FilteredAttrs { directly_changed })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_to_path_simple() {
        assert_eq!(
            position_to_repo_relative_path("pkgs/foo/default.nix:34"),
            Some("pkgs/foo/default.nix".to_string())
        );
    }

    #[test]
    fn position_to_path_no_line() {
        assert_eq!(
            position_to_repo_relative_path("pkgs/foo/default.nix"),
            Some("pkgs/foo/default.nix".to_string())
        );
    }

    #[test]
    fn position_to_path_nix_store() {
        assert_eq!(
            position_to_repo_relative_path(
                "/nix/store/abc123-source/pkgs/applications/misc/hello/default.nix:34"
            ),
            Some("pkgs/applications/misc/hello/default.nix".to_string())
        );
    }

    #[test]
    fn position_to_path_empty() {
        assert_eq!(position_to_repo_relative_path(""), None);
    }

    #[test]
    fn position_to_path_just_colon() {
        assert_eq!(position_to_repo_relative_path(":42"), None);
    }

    #[test]
    fn position_to_path_nix_store_no_subdir() {
        // Edge case: store path with no subdirectory
        assert_eq!(
            position_to_repo_relative_path("/nix/store/abc123-source"),
            None
        );
    }
}
