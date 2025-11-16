mod actions;
mod types;

use anyhow::Result;
use octocrab::models::pulls::PullRequest;
use tokio::sync::mpsc;
use tracing::warn;
pub use types::{GitRepo, GitWorkspace};

use crate::ci::RepoTask;
use crate::services::AsyncService;

#[derive(Debug, Clone)]
pub enum GitTask {
    Checkout(GitWorkspace),
    GitHubCheckout(PullRequest),
}

pub struct GitService {
    git_sender: mpsc::Sender<GitTask>,
    git_receiver: Option<mpsc::Receiver<GitTask>>,
    repo_sender: mpsc::Sender<RepoTask>,
}

impl GitService {
    pub fn new(repo_sender: mpsc::Sender<RepoTask>) -> Result<Self> {
        let (git_sender, git_receiver) = mpsc::channel(1000);

        Ok(Self {
            git_sender,
            git_receiver: Some(git_receiver),
            repo_sender,
        })
    }
}

impl AsyncService<GitTask> for GitService {
    /// Producer channel for sending build requests for eventual scheduling
    fn get_sender(&self) -> mpsc::Sender<GitTask> {
        self.git_sender.clone()
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<GitTask>> {
        self.git_receiver.take()
    }

    async fn handle_task(&self, task: GitTask) -> anyhow::Result<()> {
        match task {
            GitTask::Checkout(repo) => {
                repo.ensure_master_clone().await?;
                repo.create_worktree().await?;
                let repo_task = RepoTask::Read(repo.worktree_path());
                self.repo_sender.send(repo_task).await?;
            },
            GitTask::GitHubCheckout(pr) => {
                let owned_default_repo = pr.repo.clone().map(|x| *x).clone();
                let default_repo = owned_default_repo.as_ref();
                let base_repo = GitRepo::from_gh_repo((*pr.base).repo.as_ref(), default_repo)?;
                let head_repo = GitRepo::from_gh_repo((*pr.head).repo.as_ref(), default_repo)?;
                let repo = GitWorkspace::from_git_repo(base_repo, &pr.head.sha);
                repo.ensure_master_clone().await?;
                repo.fetch_remote_repo(&head_repo, &pr.head.ref_field)
                    .await?;
                repo.create_worktree().await?;
                let repo_task = RepoTask::ReadGitHub((repo.worktree_path(), pr));
                self.repo_sender.send(repo_task).await?;
            },
        }

        Ok(())
    }

    async fn handle_failure(&mut self, error: anyhow::Error) {
        warn!("Failure while handling git action: {:?}", error);
    }

    async fn handle_closure(&mut self) {
        warn!("Closing git management service");
    }
}
