use std::path::Path;
use std::process::Output;

use anyhow::Result;
use tokio::process::Command;
use tracing::debug;

pub async fn clone_git_repo(git_url: &str, path: &str) -> Result<Output> {
    debug!("Attempting to checkout {} at {}", git_url, path);

    let out = Command::new("git")
        .args(["clone", git_url, path])
        .output()
        .await?;

    Ok(out)
}

#[allow(dead_code)]
pub async fn repo_is_healthy(path: &str) -> Result<bool> {
    debug!("Checking if {} is a healthy git repository", &path);

    let status = Command::new("git").args(["status", &path]).status().await?;

    Ok(status.success())
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

    let out = Command::new("git")
        .current_dir(repo_dir)
        .args(["worktree", "add", "--detach", &worktree_dir, &commitish])
        .output()
        .await?;

    Ok(out)
}
