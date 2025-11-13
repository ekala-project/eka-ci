mod actions;
mod types;

use anyhow::Result;
use octocrab::models::pulls::PullRequest;
use tokio::sync::mpsc;
use tracing::warn;
pub use types::GitWorkspace;

use crate::ci::RepoTask;
use crate::services::AsyncService;

#[derive(Debug, Clone)]
pub enum GitTask {
    Checkout(GitWorkspace),
    PRCheckout((GitWorkspace, PullRequest)),
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
            GitTask::PRCheckout((repo, pr)) => {
                repo.ensure_master_clone().await?;
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
