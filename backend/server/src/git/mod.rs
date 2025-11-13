mod actions;
mod types;

use anyhow::Result;
use octocrab::models::pulls::PullRequest;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
pub use types::GitWorkspace;

use crate::ci::RepoTask;

#[derive(Debug, Clone)]
pub enum GitTask {
    Checkout(GitWorkspace),
    PRCheckout((GitWorkspace, PullRequest)),
}

pub struct GitService {
    git_sender: mpsc::Sender<GitTask>,
    git_receiver: mpsc::Receiver<GitTask>,
}

impl GitService {
    pub fn new() -> Result<Self> {
        let (git_sender, git_receiver) = mpsc::channel(1000);

        Ok(Self {
            git_sender,
            git_receiver,
        })
    }

    /// Producer channel for sending build requests for eventual scheduling
    pub fn git_request_sender(&self) -> mpsc::Sender<GitTask> {
        self.git_sender.clone()
    }

    pub fn run(
        self,
        repo_sender: mpsc::Sender<RepoTask>,
        cancel_token: CancellationToken,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            cancel_token
                .run_until_cancelled(git_tasks_loop(self.git_receiver, repo_sender))
                .await;
        })
    }
}

async fn git_tasks_loop(
    mut receiver: mpsc::Receiver<GitTask>,
    repo_sender: mpsc::Sender<RepoTask>,
) {
    loop {
        if let Some(task) = receiver.recv().await {
            debug!("Received git task {:?}", &task);
            if let Err(e) = handle_git_task(task.clone(), &repo_sender).await {
                warn!("Failed to handle ingress request {:?}: {:?}", &task, e);
            }
        }
    }
}

async fn handle_git_task(
    task: GitTask,
    repo_sender: &mpsc::Sender<RepoTask>,
) -> anyhow::Result<()> {
    match task {
        GitTask::Checkout(repo) => {
            repo.ensure_master_clone().await?;
            repo.create_worktree().await?;
            let repo_task = RepoTask::Read(repo.worktree_path());
            repo_sender.send(repo_task).await?;
        },
        GitTask::PRCheckout((repo, pr)) => {
            repo.ensure_master_clone().await?;
            repo.create_worktree().await?;
            let repo_task = RepoTask::ReadGitHub((repo.worktree_path(), pr));
            repo_sender.send(repo_task).await?;
        },
    }

    Ok(())
}
