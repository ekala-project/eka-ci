//! Reconstitute garbage-collected `.drv` files by re-evaluating the nix
//! expression that originally produced them.
//!
//! After `nix-collect-garbage`, `.drv` files are removed from the nix store.
//! This module detects missing drvs and recreates them by looking up the
//! original jobset context (commit SHA, job name, repo) and re-running
//! `nix-eval-jobs` on the same nix file. A single re-evaluation recreates
//! all top-level attrs AND their transitive dependency closures.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::db::github::types::JobSetInfo;
use crate::db::model::drv_id::DrvId;
use crate::git::{GitProtocol, GitRepo, GitWorkspace};

/// Check whether a `.drv` file exists in the nix store.
pub async fn drv_store_path_exists(store_path: &str) -> bool {
    tokio::fs::metadata(store_path).await.is_ok()
}

/// Deduplicates in-flight reconstitution so that many GC'd drvs from the
/// same evaluation trigger only one `nix-eval-jobs` invocation.
pub struct ReconstitutionTracker {
    in_flight: Mutex<HashSet<String>>,
}

impl ReconstitutionTracker {
    pub fn new() -> Self {
        Self {
            in_flight: Mutex::new(HashSet::new()),
        }
    }

    fn key(sha: &str, job: &str) -> String {
        format!("{}/{}", sha, job)
    }

    /// Try to acquire the reconstitution lock for a (sha, job).
    /// Returns `true` if we acquired it (caller should run eval),
    /// `false` if another task is already reconstituting this eval.
    async fn try_acquire(&self, sha: &str, job: &str) -> bool {
        self.in_flight.lock().await.insert(Self::key(sha, job))
    }

    async fn release(&self, sha: &str, job: &str) {
        self.in_flight.lock().await.remove(&Self::key(sha, job));
    }
}

/// Attempt to reconstitute a garbage-collected drv by re-evaluating the
/// nix expression that originally produced it.
///
/// Returns `Ok(true)` if the drv now exists in the store, `Ok(false)` if
/// reconstitution was not possible (no jobset context found), or `Err` on
/// failure.
pub async fn reconstitute_drv(
    drv_id: &DrvId,
    pool: &SqlitePool,
    tracker: &ReconstitutionTracker,
) -> Result<bool> {
    // Step 1: Find the jobset context (direct job member first, then transitive)
    let context = match crate::db::github::get_reconstitution_context(drv_id, pool).await? {
        Some(ctx) => ctx,
        None => {
            match crate::db::github::get_reconstitution_context_transitive(drv_id, pool).await? {
                Some(ctx) => ctx,
                None => {
                    debug!(
                        "no reconstitution context for {}, cannot reconstitute",
                        drv_id.store_path()
                    );
                    return Ok(false);
                },
            }
        },
    };

    // Step 2: Deduplicate — skip if already in-flight for this (sha, job)
    if !tracker.try_acquire(&context.sha, &context.job).await {
        debug!(
            "reconstitution already in-flight for {}/{}",
            context.sha, context.job
        );
        // Wait for the in-flight reconstitution to finish, then check
        tokio::time::sleep(Duration::from_secs(10)).await;
        return Ok(drv_store_path_exists(&drv_id.store_path()).await);
    }

    let result = do_reconstitute(&context, drv_id).await;
    tracker.release(&context.sha, &context.job).await;
    result
}

async fn do_reconstitute(context: &JobSetInfo, drv_id: &DrvId) -> Result<bool> {
    info!(
        "reconstituting GC'd drv {} via re-evaluation of job '{}' at {}/{} commit {}",
        drv_id.store_path(),
        context.job,
        context.owner,
        context.repo_name,
        &context.sha[..12.min(context.sha.len())]
    );

    // Step 3: Ensure the git worktree exists
    let repo = GitRepo {
        protocol: GitProtocol::Https,
        domain: "github.com".to_string(),
        owner: context.owner.clone(),
        repo: context.repo_name.clone(),
    };
    let workspace = GitWorkspace::from_git_repo(repo, &context.sha);
    workspace
        .ensure_master_clone()
        .await
        .context("failed to ensure master clone for reconstitution")?;
    workspace
        .create_worktree()
        .await
        .context("failed to create worktree for reconstitution")?;

    // Step 4: Read .ekaci/config.json to find the nix file for this job
    let worktree_path = workspace.worktree_path();
    let config_path = worktree_path.join(".ekaci").join("config.json");
    let config_contents = tokio::fs::read_to_string(&config_path)
        .await
        .with_context(|| {
            format!(
                "failed to read {} for reconstitution",
                config_path.display()
            )
        })?;
    let config: ci_config::CIConfig =
        serde_json::from_str(&config_contents).context("failed to parse .ekaci/config.json")?;

    let job_config = config
        .jobs
        .get(&context.job)
        .with_context(|| format!("job '{}' not found in .ekaci/config.json", context.job))?;

    // Step 5: Resolve the nix file path
    let file_path = crate::ci::resolve_file_path(
        worktree_path.clone(),
        config_path,
        job_config.file.clone(),
    )
    .context("failed to resolve nix file path for reconstitution")?;

    // Step 6: Run nix-eval-jobs to repopulate the nix store with .drv files
    run_nix_eval_jobs_for_reconstitution(&file_path).await?;

    // Step 7: Verify the target drv now exists
    let exists = drv_store_path_exists(&drv_id.store_path()).await;
    if exists {
        info!("successfully reconstituted {}", drv_id.store_path());
    } else {
        warn!(
            "reconstitution completed but {} still not in store",
            drv_id.store_path()
        );
    }

    Ok(exists)
}

/// Run `nix-eval-jobs` purely to repopulate `.drv` files in the store.
/// We discard the output — we only care about the side effect of
/// `.drv` files being created during evaluation.
async fn run_nix_eval_jobs_for_reconstitution(file_path: &PathBuf) -> Result<()> {
    let file_path_str = file_path.to_string_lossy();
    debug!(
        "running nix-eval-jobs for reconstitution: {}",
        file_path_str
    );

    let output = tokio::time::timeout(
        Duration::from_secs(600), // 10 minute timeout
        tokio::process::Command::new("nix-eval-jobs")
            .args(["--show-input-drvs", "--meta", &file_path_str])
            .output(),
    )
    .await
    .context("nix-eval-jobs timed out during reconstitution")?
    .context("failed to spawn nix-eval-jobs for reconstitution")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            "nix-eval-jobs exited with non-zero status during reconstitution: {}",
            stderr.lines().take(5).collect::<Vec<_>>().join("\n")
        );
        // Non-zero exit is not necessarily fatal — nix-eval-jobs may have
        // partially succeeded and created the drv we need. The caller will
        // check drv existence afterwards.
    }

    Ok(())
}
