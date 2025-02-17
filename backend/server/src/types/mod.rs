use shared::types as t;
use std::process::Command;

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

impl PREvalInfo {
    fn worktree_name(&self) -> String {
        let dir = format!("{}-{}-{}", self.domain, self.owner, self.repo);
        // TODO: Improve sanitizaiton
        dir.replace(".", "-")
    }

    fn git_ssh_url(&self) -> String {
        format!("git@{}/{}/{}.git", self.domain, self.owner, self.repo)
    }

    fn git_https_url(&self) -> String {
        format!("https://{}/{}/{}", self.domain, self.owner, self.repo)
    }

    /// Attempt to checkout
    pub fn checkout(&self) {
        // Check if repo has already been pulled down
        let mut worktree_dir = shared::dirs::eka_git_tree_dirs();
        worktree_dir.push(self.worktree_name());
        if !worktree_dir.exists() {
            // TODO: git clone
            // git clone "git_ssh_url" "path/to/default"
        }

        let head_tree = worktree_dir.clone();
        head_tree.push(&self.head_commit);
        let base_tree = worktree_dir.clone();
        base_tree.push(&self.base_commit);


        // Create worktree for head branch (if it doesn't already exist)

        // Create worktree for base branch (if it doesn't already exist)
    }
}
