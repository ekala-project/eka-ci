use std::collections::HashMap;

use anyhow::{Context, Result};
use octocrab::Octocrab;
use octocrab::models::pulls::PullRequest;
use octocrab::models::{CheckRunId, Installation};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;
use crate::nix::nix_eval_jobs::NixEvalDrv;

mod actions;

#[derive(Debug, Clone)]
/// Information needed to create a CI check run gate
pub struct CICheckInfo {
    pub commit: String,
    pub base_commit: String,
    pub owner: String,
    pub repo_name: String,
}

impl CICheckInfo {
    pub fn from_gh_pr(pr: &PullRequest) -> Self {
        let commit = pr.head.sha.clone();
        let base_commit = pr.base.sha.clone();
        let repo = (*pr.head).repo.as_ref().unwrap();
        let owner = repo.owner.as_ref().unwrap().login.clone();
        let repo_name = repo.name.clone();

        Self {
            commit,
            base_commit,
            owner,
            repo_name,
        }
    }
}

#[derive(Debug)]
pub enum GitHubTask {
    UpdateBuildStatus {
        drv_id: DrvId,
        status: DrvBuildState,
    },
    CreateJobSet {
        ci_check_info: CICheckInfo,
        name: String,
        jobs: Vec<NixEvalDrv>,
    },
    CreateCIConfigureGate {
        ci_check_info: CICheckInfo,
    },
    CompleteCIConfigureGate {
        ci_check_info: CICheckInfo,
    },
}

type Owner = String;
type Commit = String;

/// This service will be response for pushing/posting events to GitHub
pub struct GitHubService {
    // Channels to individual services
    // We may in the future need to recover an individual service, so retaining
    // a handle to the other service channels will be prequisite
    db_service: DbService,
    octocrab: Octocrab,
    installations: HashMap<Owner, Installation>,
    github_receiver: mpsc::Receiver<GitHubTask>,
    github_sender: mpsc::Sender<GitHubTask>,
    github_configure_checks: HashMap<Commit, CheckRunId>,
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
            installations.insert(installation.account.login.clone(), installation);
        }

        debug!("Installations: {:?}", &installations);

        let (github_sender, github_receiver) = mpsc::channel(100);
        Ok(Self {
            db_service,
            octocrab,
            installations,
            github_receiver,
            github_sender,
            github_configure_checks: HashMap::new(),
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

    /// Attempt to look up installation by owner
    fn octocrab_for_owner(&self, owner: &str) -> Result<Octocrab> {
        let installation = self
            .installations
            .get(owner)
            .context("No installation associated with owner")?;
        debug!(
            "Found installation for owner {}: {:?}",
            &owner, &installation
        );
        let octo = self.octocrab.installation(installation.id)?;
        Ok(octo)
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

    async fn handle_github_task(&mut self, task: &GitHubTask) -> Result<()> {
        use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

        match task {
            GitHubTask::UpdateBuildStatus { drv_id, status } => {
                let check_runs = self.db_service.check_runs_for_drv_path(drv_id).await?;

                for check_run in check_runs {
                    let octocrab = self.octocrab_for_owner(&check_run.repo_owner)?;
                    let (gh_status, gh_conclusion) = status.as_gh_checkrun_state();

                    let check_builder =
                        octocrab.checks(&check_run.repo_owner, &check_run.repo_name);
                    let mut check_update = check_builder
                        .update_check_run(octocrab::models::CheckRunId(
                            check_run.check_run_id as u64,
                        ))
                        .status(gh_status);

                    if let Some(conclusion) = gh_conclusion {
                        check_update = check_update.conclusion(conclusion);
                    }

                    check_update.send().await?;
                }
            },
            GitHubTask::CreateJobSet {
                ci_check_info,
                name,
                jobs,
            } => {
                use tracing::info;

                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                info!(
                    "Inserting {} jobs for {}",
                    jobs.len(),
                    &ci_check_info.commit
                );
                let job_pairs: Vec<(String, NixEvalDrv)> = jobs
                    .iter()
                    .map(|x| (ci_check_info.commit.clone(), (*x).clone()))
                    .collect();
                self.db_service
                    .create_github_jobset_with_jobs(&ci_check_info.commit, &name, &job_pairs)
                    .await?;

                let new_jobs = self
                    .db_service
                    .new_jobs(&ci_check_info.commit, &ci_check_info.base_commit, &name)
                    .await?;

                // TODO: parallelize this, and make it async
                info!("Emitting {} jobs for {}", jobs.len(), &ci_check_info.commit);
                for job in new_jobs {
                    // TODO: Make this more efficient.
                    // We want the attrpath as the "ci check run", but the drv doesn't
                    // actually have this information, however, the results from nix_eval_jobs does
                    let maybe_eval_job = jobs
                        .iter()
                        .find(|&x| x.drv_path == job.drv_path.store_path());

                    if let Some(eval_job) = maybe_eval_job {
                        let check_run = octocrab
                            .checks(&ci_check_info.owner, &ci_check_info.repo_name)
                            .create_check_run(
                                &format!("{} / jobs:{name}", eval_job.attr),
                                &ci_check_info.commit,
                            )
                            .status(CheckRunStatus::InProgress)
                            .send()
                            .await?;

                        self.db_service
                            .insert_check_run_info(
                                check_run.id.0 as i64,
                                &job.drv_path,
                                &ci_check_info.repo_name,
                                &ci_check_info.owner,
                            )
                            .await?;
                    } else {
                        warn!(
                            "Failed to find eval_job for {}:",
                            &job.drv_path.store_path()
                        );
                    }
                }
            },
            GitHubTask::CreateCIConfigureGate { ci_check_info } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                let check_run = actions::create_ci_configure_gate(&octocrab, ci_check_info).await?;
                self.github_configure_checks
                    .insert(ci_check_info.commit.clone(), check_run.id);
            },
            GitHubTask::CompleteCIConfigureGate { ci_check_info } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                let check_run_id = self
                    .github_configure_checks
                    .get(&ci_check_info.commit)
                    .context("No configure gate check run found for commit")?;
                actions::update_ci_configure_gate(
                    &octocrab,
                    ci_check_info,
                    *check_run_id,
                    CheckRunStatus::Completed,
                    CheckRunConclusion::Success,
                )
                .await?;
            },
        }
        Ok(())
    }
}
