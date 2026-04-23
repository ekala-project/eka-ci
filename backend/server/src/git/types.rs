use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use octocrab::models::Repository;
use shared::types::GitRequest;
use tracing::{debug, warn};

use super::actions::{add_git_worktree, clone_git_repo, fetch_remote_repo};

/// RAII guard that removes `path` on drop unless `disarm()` is called.
///
/// Used to keep multi-step git checkouts cancellation-safe: if the
/// owning future is dropped mid-`git clone` / `git worktree add` the
/// child process is killed and leaves a partial directory; this guard
/// removes it so the next attempt starts clean.
struct CleanupGuard {
    path: Option<PathBuf>,
    /// If set, run `git worktree prune` in this repo on drop (best-effort)
    /// so stale worktree admin entries do not accumulate.
    prune_in: Option<PathBuf>,
}

impl CleanupGuard {
    fn dir(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            prune_in: None,
        }
    }

    fn worktree(worktree_path: PathBuf, repo_path: PathBuf) -> Self {
        Self {
            path: Some(worktree_path),
            prune_in: Some(repo_path),
        }
    }

    fn disarm(mut self) {
        self.path.take();
        self.prune_in.take();
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if let Some(p) = self.path.take() {
            warn!(path = %p.display(), "removing partial checkout after cancellation or failure");
            if let Err(e) = std::fs::remove_dir_all(&p) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!(path = %p.display(), error = %e, "failed to clean up partial checkout");
                }
            }
        }
        if let Some(repo) = self.prune_in.take() {
            // Best-effort: clean up the git-internal worktree admin entry
            // so a later `git worktree add` at the same path succeeds.
            let _ = std::process::Command::new("git")
                .current_dir(&repo)
                .args(["worktree", "prune"])
                .output();
        }
    }
}

fn path_to_str(p: &Path) -> Result<&str> {
    p.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", p.display()))
}

/// Return the directory that contains all ekaci-managed git checkouts.
/// Every worktree and master clone lives underneath this directory, so
/// it serves as the containment boundary for path-safety checks before
/// `nix eval` / `nix-eval-jobs` are invoked on PR-influenced paths.
///
/// In tests, the root can be overridden by setting the
/// `EKACI_WORKSPACE_ROOT` environment variable to an absolute path; this
/// lets tests exercise containment logic without polluting
/// `~/.local/share/ekaci/`.
pub fn workspace_root() -> Result<PathBuf> {
    if let Ok(override_root) = std::env::var("EKACI_WORKSPACE_ROOT") {
        return Ok(PathBuf::from(override_root));
    }
    let dirs = xdg::BaseDirectories::with_prefix("ekaci")
        .context("failed to locate XDG base directories for ekaci")?;
    Ok(dirs.get_data_home().join("repos"))
}

#[derive(Clone, Debug)]
pub enum GitProtocol {
    Https,
    // Supported by `protocol_prefix`, but no construction site yet.
    // Retained for future configuration (e.g. self-hosted forges over plain HTTP).
    #[allow(dead_code)]
    Http,
    // Supported by `protocol_prefix`, but no construction site yet.
    // Retained for future SSH protocol support.
    #[allow(dead_code)]
    Ssh,
}

impl GitProtocol {
    /// Which protocol protocol prefix to use when constructing a git checkout url
    pub fn protocol_prefix(&self) -> &str {
        match self {
            Self::Https => "https://",
            Self::Http => "http://",
            Self::Ssh => "git@",
        }
    }
}

/// Information to checkout a repository and a specific branch or commit
#[derive(Clone, Debug)]
pub struct GitRepo {
    pub protocol: GitProtocol,
    pub domain: String,
    pub owner: String,
    pub repo: String,
}

impl GitRepo {
    pub fn from_gh_repo(repo: Repository) -> Result<Self> {
        Ok(Self {
            protocol: GitProtocol::Https,
            domain: repo
                .html_url
                .context("missing html_url")?
                .domain()
                .context("Missing domain")?
                .to_string(),
            owner: repo.owner.as_ref().context("Missing owner")?.login.clone(),
            repo: repo.name.clone(),
        })
    }
}

impl GitRepo {
    pub fn checkout_url(&self) -> String {
        match self.protocol {
            GitProtocol::Ssh => {
                format!(
                    "{}{}:{}/{}.git",
                    self.protocol.protocol_prefix(),
                    &self.domain,
                    &self.owner,
                    &self.repo
                )
            },
            _ => {
                format!(
                    "{}{}/{}/{}.git",
                    self.protocol.protocol_prefix(),
                    &self.domain,
                    &self.owner,
                    &self.repo
                )
            },
        }
    }
}

/// This is meant to provision a worktree
/// To quicken checkout performance, we use a single "origin" to keep
/// a running heap of git objects, then we can just use worktrees to create
/// cheap per-commit directories
#[derive(Clone, Debug)]
pub struct GitWorkspace {
    repo: GitRepo,
    /// Branch, tag, or commit
    rev_parse: String,
    /// Non-worktree tree path
    repo_path: PathBuf,
    /// Worktree tree path
    worktree_path: PathBuf,
}

impl GitWorkspace {
    pub fn new(repo: GitRepo, rev_parse: &str, mut repo_path_prefix: PathBuf) -> Self {
        repo_path_prefix.push(&repo.domain);
        repo_path_prefix.push(&repo.owner);
        repo_path_prefix.push(&repo.repo);

        let mut worktree_path = repo_path_prefix.clone();
        worktree_path.push("worktrees");
        worktree_path.push(rev_parse);

        repo_path_prefix.push("master");
        Self {
            repo,
            rev_parse: rev_parse.to_string(),
            repo_path: repo_path_prefix,
            worktree_path,
        }
    }

    pub fn from_git_repo(repo: GitRepo, rev_parse: &str) -> Self {
        let dirs = xdg::BaseDirectories::with_prefix("ekaci").unwrap();
        let repos_dir = dirs.create_data_directory("repos").unwrap();
        Self::new(repo, rev_parse, repos_dir)
    }

    /// Ensure that the main clone exists and is healthy.
    ///
    /// A `CleanupGuard` removes the target dir if the clone is
    /// cancelled (future dropped) or exits non-zero, so the next
    /// attempt starts from a clean slate instead of reusing a
    /// half-populated repo.
    pub async fn ensure_master_clone(&self) -> anyhow::Result<()> {
        if self.repo_path.exists() {
            return Ok(());
        }
        debug!("{:?} does not exist, creating", &self.repo_path);
        std::fs::create_dir_all(self.repo_path.parent().unwrap())?;

        let dest_dir = path_to_str(&self.repo_path)?.to_string();
        let guard = CleanupGuard::dir(self.repo_path.clone());
        let out = clone_git_repo(&self.repo.checkout_url(), &dest_dir).await?;
        if !out.status.success() {
            bail!(
                "git clone of {} failed: {}",
                self.repo.checkout_url(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        guard.disarm();
        Ok(())
    }

    /// Assumes that master worktree has already been instantiated
    pub async fn fetch_remote_repo(
        &self,
        remote_repo: &GitRepo,
        branch: &str,
    ) -> anyhow::Result<()> {
        fetch_remote_repo(&self.repo_path, remote_repo, branch).await
    }

    pub async fn create_worktree(&self) -> anyhow::Result<()> {
        self.ensure_master_clone().await?;

        if self.worktree_path.exists() {
            return Ok(());
        }
        std::fs::create_dir_all(self.worktree_path.parent().unwrap())?;

        let dest_dir = path_to_str(&self.worktree_path)?.to_string();
        // Worktree guard also prunes the parent repo's admin entry on drop.
        let guard = CleanupGuard::worktree(self.worktree_path.clone(), self.repo_path.clone());
        let out = add_git_worktree(&self.repo_path, &dest_dir, &self.rev_parse).await?;
        if !out.status.success() {
            bail!(
                "git worktree add at {} failed: {}",
                dest_dir,
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        guard.disarm();
        Ok(())
    }

    pub fn worktree_path(&self) -> PathBuf {
        self.worktree_path.clone()
    }

    pub fn from_git_request(request: GitRequest) -> Self {
        let repo = GitRepo {
            protocol: GitProtocol::Https,
            domain: request.domain,
            owner: request.owner,
            repo: request.repo,
        };

        Self::from_git_repo(repo, &request.commitish)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssh_repo_url() {
        let repo = GitRepo {
            protocol: GitProtocol::Ssh,
            domain: "github.com".to_string(),
            owner: "XAMPPRocky".to_string(),
            repo: "octocrab".to_string(),
        };

        let expected_url = "git@github.com:XAMPPRocky/octocrab.git";
        assert_eq!(repo.checkout_url(), expected_url);
    }

    #[test]
    fn test_https_repo_url() {
        let repo = GitRepo {
            protocol: GitProtocol::Https,
            domain: "github.com".to_string(),
            owner: "XAMPPRocky".to_string(),
            repo: "octocrab".to_string(),
        };

        let expected_url = "https://github.com/XAMPPRocky/octocrab.git";
        assert_eq!(repo.checkout_url(), expected_url);
    }
}

#[cfg(test)]
mod cleanup_guard_tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn populate_dir(path: &Path) {
        fs::create_dir_all(path).unwrap();
        fs::write(path.join("partial.txt"), b"half-written").unwrap();
    }

    #[test]
    fn drop_removes_target_when_armed() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("partial-clone");
        populate_dir(&target);
        assert!(target.exists());

        let guard = CleanupGuard::dir(target.clone());
        drop(guard);

        assert!(
            !target.exists(),
            "armed guard must remove partial dir on drop"
        );
    }

    #[test]
    fn disarm_preserves_target() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("committed-clone");
        populate_dir(&target);

        let guard = CleanupGuard::dir(target.clone());
        guard.disarm();

        assert!(
            target.exists(),
            "disarmed guard must leave successful checkout in place"
        );
        assert!(target.join("partial.txt").exists());
    }

    #[test]
    fn drop_tolerates_missing_target() {
        // If the child process never managed to create the dir, drop must not panic.
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("never-created");
        assert!(!target.exists());

        let guard = CleanupGuard::dir(target.clone());
        drop(guard);
    }

    #[test]
    fn worktree_guard_removes_worktree_dir_on_drop() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let worktree = tmp.path().join("worktree");
        fs::create_dir_all(&repo).unwrap();
        populate_dir(&worktree);

        let guard = CleanupGuard::worktree(worktree.clone(), repo.clone());
        drop(guard);

        assert!(!worktree.exists());
        // Repo dir itself is unaffected (prune is best-effort, no repo → no-op).
        assert!(repo.exists());
    }

    #[test]
    fn worktree_guard_disarm_preserves_both() {
        let tmp = TempDir::new().unwrap();
        let repo = tmp.path().join("repo");
        let worktree = tmp.path().join("worktree");
        fs::create_dir_all(&repo).unwrap();
        populate_dir(&worktree);

        let guard = CleanupGuard::worktree(worktree.clone(), repo.clone());
        guard.disarm();

        assert!(worktree.exists());
        assert!(repo.exists());
    }
}
