use std::path::Path;
use std::process::Output;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;
use tracing::debug;

use super::GitRepo;

/// Timeout for git clone operations (large repos may take a while).
const GIT_CLONE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Timeout for git fetch operations.
const GIT_FETCH_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Timeout for git worktree operations (quick local operation).
const GIT_WORKTREE_TIMEOUT: Duration = Duration::from_secs(60);

pub async fn clone_git_repo(git_url: &str, path: &str) -> Result<Output> {
    debug!("Attempting to checkout {} at {}", git_url, path);

    let out = tokio::time::timeout(
        GIT_CLONE_TIMEOUT,
        Command::new("git").args(["clone", git_url, path]).output(),
    )
    .await
    .context("git clone timed out")?
    .context("failed to execute git clone")?;

    Ok(out)
}

pub async fn fetch_remote_repo<P: AsRef<Path>>(
    repo_dir: P,
    repo: &GitRepo,
    reference: &str,
) -> Result<()> {
    debug!("fetching branch {} from {}", reference, repo.checkout_url());

    let out = tokio::time::timeout(
        GIT_FETCH_TIMEOUT,
        Command::new("git")
            .current_dir(repo_dir)
            .args(["fetch", &repo.checkout_url(), reference])
            .output(),
    )
    .await
    .context("git fetch timed out")?
    .context("failed to execute git fetch")?;

    if !out.status.success() {
        anyhow::bail!("Failed to fetch remote branch {}", repo.checkout_url());
    }

    Ok(())
}
pub async fn add_git_worktree<P: AsRef<Path>>(
    repo_dir: P,
    worktree_dir: &str,
    commitish: &str,
) -> Result<Output> {
    debug!(
        "Creating worktree at {} on commit {}",
        worktree_dir, commitish
    );

    let out = tokio::time::timeout(
        GIT_WORKTREE_TIMEOUT,
        Command::new("git")
            .current_dir(repo_dir)
            .args(["worktree", "add", "--detach", worktree_dir, commitish])
            .output(),
    )
    .await
    .context("git worktree add timed out")?
    .context("failed to execute git worktree add")?;

    Ok(out)
}
