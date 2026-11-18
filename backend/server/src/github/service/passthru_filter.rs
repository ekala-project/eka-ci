//! Filter changed packages to only those directly modified in a PR.
//!
//! Uses `meta.position` (package source file location) cross-referenced
//! with `git diff --name-only` to distinguish packages whose definition
//! was edited from packages that merely got rebuilt due to a dependency
//! change.

use std::path::Path;

use anyhow::Result;
use sqlx::{Pool, Sqlite};
use tracing::{debug, warn};

use crate::db::github::NewOrChangedJob;
use crate::db::model::DrvId;
use crate::nix::strategy::{position_file, repo_roots};

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

/// Filter changed jobs to only those whose source file was directly
/// modified in this PR.
///
/// Returns attr names that should have their `passthru.tests` evaluated.
pub(crate) async fn filter_directly_changed(
    changed_jobs: &[NewOrChangedJob],
    base_sha: &str,
    head_sha: &str,
    repo_dir: &Path,
    pool: &Pool<Sqlite>,
) -> Result<FilteredAttrs> {
    if changed_jobs.is_empty() {
        return Ok(FilteredAttrs {
            directly_changed: Vec::new(),
        });
    }

    // Get changed files from git
    let changed_files = crate::git::diff_changed_files(repo_dir, base_sha, head_sha).await?;

    if changed_files.is_empty() {
        return Ok(FilteredAttrs {
            directly_changed: Vec::new(),
        });
    }

    // Get meta_positions for all changed drv_paths
    let drv_paths: Vec<&DrvId> = changed_jobs.iter().map(|j| &j.drv_path).collect();
    let positions = get_meta_positions(&drv_paths, pool).await?;

    // Positions point into the evaluated head worktree.
    let roots = repo_roots(&repo_dir.join("worktrees").join(head_sha));

    // Build a lookup from drv_path -> meta_position
    let position_map: std::collections::HashMap<String, Option<String>> =
        positions.into_iter().collect();

    // Filter: keep attrs whose meta.position file is in the changed set
    let mut directly_changed = Vec::new();
    for job in changed_jobs {
        let drv_str = job.drv_path.to_string();
        let meta_pos = position_map.get(&drv_str).and_then(|p| p.as_deref());

        if let Some(pos) = meta_pos {
            if let Some(rel_path) = position_file(pos, &roots) {
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
