use std::collections::HashMap;

use anyhow::Result;
use octocrab::Octocrab;
use octocrab::models::pulls::PullRequest;
use octocrab::models::{Installation, InstallationId};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::nix::nix_eval_jobs::NixEvalDrv;

#[derive(Debug)]
pub enum GitHubTask {
    CreateJobSet {
        pr: PullRequest,
        jobs: Vec<NixEvalDrv>,
    },
}

/// This service will be response for pushing/posting events to GitHub
pub struct GitHubService {
    // Channels to individual services
    // We may in the future need to recover an individual service, so retaining
    // a handle to the other service channels will be prequisite
    db_service: DbService,
    octocrab: Octocrab,
    // TODO: look up installation associated with webhook event
    #[allow(dead_code)]
    installations: HashMap<InstallationId, Installation>,
    github_receiver: mpsc::Receiver<GitHubTask>,
    github_sender: mpsc::Sender<GitHubTask>,
}

impl GitHubService {
    pub async fn new(db_service: DbService, octocrab: Octocrab) -> anyhow::Result<Self> {
        use futures::stream::TryStreamExt;
        use tokio::pin;

        let mut installations = HashMap::new();
        let octoclone = octocrab.clone();
        let installations_stream = octocrab
            .apps()
            .installations()
            .send()
            .await?
            .into_stream(&octoclone);
        pin!(installations_stream);
        while let Some(installation) = installations_stream.try_next().await? {
            installations.insert(installation.id, installation);
        }

        debug!("Installations: {:?}", &installations);

        let (github_sender, github_receiver) = mpsc::channel(100);
        Ok(Self {
            db_service,
            octocrab,
            installations,
            github_receiver,
            github_sender,
        })
    }

    pub fn get_sender(&self) -> mpsc::Sender<GitHubTask> {
        self.github_sender.clone()
    }

    pub fn run(self, cancel_token: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            cancel_token
                .run_until_cancelled(self.github_tasks_loop())
                .await;
        })
    }

    async fn github_tasks_loop(mut self) {
        loop {
            if let Some(task) = self.github_receiver.recv().await {
                debug!("Received github task {:?}", &task);
                if let Err(e) = self.handle_github_task(&task).await {
                    warn!("Failed to handle github request {:?}: {:?}", &task, e);
                }
            }
        }
    }

    async fn handle_github_task(&self, task: &GitHubTask) -> Result<()> {
        match task {
            GitHubTask::CreateJobSet { pr, jobs } => {
                use octocrab::params::checks::CheckRunStatus;

                let commit = pr.head.sha.clone();
                let job_pairs: Vec<(String, NixEvalDrv)> = jobs
                    .iter()
                    .map(|x| (commit.clone(), (*x).clone()))
                    .collect();
                self.db_service.insert_jobset(&job_pairs).await?;
                let repo = *pr.repo.as_ref().unwrap().clone();
                let owner = repo.owner.unwrap().login;
                let repo_name = repo.name;

                // TODO: parallelize this
                for job in jobs {
                    self.octocrab
                        .checks(&owner, &repo_name)
                        .create_check_run(&job.attr, &commit)
                        .status(CheckRunStatus::InProgress)
                        .send()
                        .await?;
                }
            },
        }
        Ok(())
    }
}
