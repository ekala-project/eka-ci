use shared::types as t;
use crate::error as err;
use crate::error::LogResult;
use std::process::{Command, ExitStatus};
use std::path::Path;

// TODO: Make serializable, use hash as ID
#[derive(Hash)]
pub(crate) struct PREvalInfo {
    /// Commit of PR branch's HEAD
    pub head_commit: String,
    /// Commit of target base branch
    pub base_commit: String,

    pub domain: String,
    pub owner: String,
    pub repo: String,
}

trait IntoEvalInfo {
    fn resolve(self) -> PREvalInfo where Self: Sized;
}

impl IntoEvalInfo for t::EvalPRRequest {
    fn resolve(self) -> PREvalInfo where Self: Sized {
        // TODO: Resovle PR branches to their underlying
        // commits
        PREvalInfo {
            head_commit: "".to_string(),
            base_commit: "".to_string(),
            domain: self.domain,
            owner: self.owner,
            repo: self.repo,
        }
    }
}

fn git_clone(url: &str, dest_dir: &str) -> err::Result<ExitStatus> {
    use std::process::Command;

    log::info!("Cloning {} to {}", &url, &dest_dir);
    Command::new("git")
        .args(&[ "clone", &url, &dest_dir])
        .status()
        .map_err(|e| e.into())
}

fn git_force_fetch<P: AsRef<Path>>(dir: P) -> err::Result<ExistStatus> {
    git -c fetch.prune=false fetch --no-tags --force https://github.com/NixOS/nixpkgs master:refs/nixpkgs-review/0 pull/384947/head:refs/nixpkgs-review/1
    Command::new("git")
        .current_dir(dir)
        .args(&[ "-c", "fetch.prune=false", "fetch", "--no-tags"])
        .status()
        .map_err(|e| e.into())
}

impl PREvalInfo {
    fn worktree_name(&self) -> String {
        let dir = format!("{}-{}-{}", self.domain, self.owner, self.repo);
        // TODO: Improve sanitizaiton
        dir.chars()
            .map(|x| match x {
                '.' =>  '-',
                '?' =>  '-',
                '/' =>  '-',
                '+' =>  '-',
                '\\' =>  '-',
                _ => x
            }).collect()
    }

    fn repo_dir(&self) -> PathBuf {
        let mut dir = shared::dirs::eka_git_tree_dirs();
        dir.push("repos");
        dir.push(self.worktree_name());
        dir
    }

    fn default_branch_dir(&self) -> PathBuf {
        let mut dir = self.repo_dir();
        dir.push("default");
        dir
    }

    fn worktree_dir(&self) -> PathBuf {
        let mut dir = self.repo_dir();
        dir.push("worktrees");
        dir
    }

    /// File used to test if a repo has already been successfully checkedout
    fn already_checkedout_file(&self) -> PathBuf {
        let mut dir = self.repo_dir();
        dir.push("already_checkedout");
        dir
    }

    /// This directory houses the main checkout of the repo
    fn ensure_default_dir(&self) -> err::Result<()> {
        let mut already_checkedout = self.already_checkedout_file();
        if !already_checkedout.exists() {
            let dest_dir = self.default_branch_dir().to_str().expect("invalid worktree dir");
            git_clone(&self.git_ssh_url(), &dest_dir)?;
            log::info!("Successfully cloned {} to {}", &self.git_ssh_url(), &dest_dir);
            std::fs::OpenOptions::new().write(true).create_new(true).open(&already_checkedout);
        } else {
            log::debug!("Skipping checkout of {}, already exists", &self.git_ssh_url());
        }

        Ok(())
    }

    fn git_ssh_url(&self) -> String {
        format!("git@{}/{}/{}.git", self.domain, self.owner, self.repo)
    }

    fn git_https_url(&self) -> String {
        format!("https://{}/{}/{}", self.domain, self.owner, self.repo)
    }

    /// Attempt to checkout
    pub fn checkout(&self) -> err::Result<()> {
        self.ensure_default_dir()?;

        let mut head_tree = worktree_dir.clone();
        head_tree.push(&self.head_commit);
        let mut base_tree = worktree_dir.clone();
        base_tree.push(&self.base_commit);


        // Create worktree for head branch (if it doesn't already exist)

        // Create worktree for base branch (if it doesn't already exist)
        return Ok(())
    }
}
