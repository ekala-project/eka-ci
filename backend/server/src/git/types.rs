use std::path::PathBuf;

use shared::types::GitRequest;
use tracing::debug;

use super::actions::{add_git_worktree, clone_git_repo};

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum GitProtocol {
    Https,
    Http,
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

impl GitWorkspace {
    pub fn new(repo: GitRepo, rev_parse: String, mut repo_path_prefix: PathBuf) -> Self {
        repo_path_prefix.push(&repo.domain);
        repo_path_prefix.push(&repo.owner);
        repo_path_prefix.push(&repo.repo);

        let mut worktree_path = repo_path_prefix.clone();
        worktree_path.push("worktrees");
        worktree_path.push(&rev_parse);

        repo_path_prefix.push("master");
        Self {
            repo,
            rev_parse,
            repo_path: repo_path_prefix,
            worktree_path,
        }
    }

    /// Ensure that the main clone exists and is healthy
    pub async fn ensure_master_clone(&self) -> anyhow::Result<()> {
        if !self.repo_path.exists() {
            debug!("{:?} does not exist, creating", &self.repo_path);
            std::fs::create_dir_all(&self.repo_path.parent().unwrap())?;
            let dest_dir = self
                .repo_path
                .clone()
                .into_os_string()
                .into_string()
                .unwrap();
            clone_git_repo(&self.repo.checkout_url(), &dest_dir).await?;
        }

        Ok(())
    }

    pub async fn create_worktree(&self) -> anyhow::Result<()> {
        self.ensure_master_clone().await?;

        if !self.worktree_path.exists() {
            std::fs::create_dir_all(&self.worktree_path.parent().unwrap())?;
            let dest_dir = self
                .worktree_path
                .clone()
                .into_os_string()
                .into_string()
                .unwrap();
            add_git_worktree(&self.repo_path, &dest_dir, &self.rev_parse).await?;
        }

        Ok(())
    }

    pub fn worktree_path(&self) -> PathBuf {
        self.worktree_path.clone()
    }

    pub fn from_git_request(request: GitRequest, repo_path_prefix: PathBuf) -> Self {
        let repo = GitRepo {
            protocol: GitProtocol::Https,
            domain: request.domain,
            owner: request.owner,
            repo: request.repo,
        };

        Self::new(repo, request.commitish, repo_path_prefix)
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
