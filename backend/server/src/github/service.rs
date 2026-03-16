use std::collections::HashMap;

use anyhow::{Context, Result};
use octocrab::Octocrab;
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
mod types;

pub use types::{CICheckInfo, Commit, GitHubTask, JobDifference, Owner};

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
    github_eval_checks: HashMap<(Commit, String), CheckRunId>,
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
            github_eval_checks: HashMap::new(),
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
                if let Err(e) = self.handle_github_task(&task).await {
                    warn!("Failed to handle github request {:?}: {:?}", &task, e);
                }
            }
        }
    }

    async fn create_job_set(
        &mut self,
        ci_check_info: &CICheckInfo,
        name: &str,
        jobs: &[NixEvalDrv],
        push_command: Option<&str>,
    ) -> Result<()> {
        let jobset_id = self
            .db_service
            .create_github_jobset_with_jobs(
                &ci_check_info.commit,
                &name,
                &ci_check_info.owner,
                &ci_check_info.repo_name,
                &jobs,
                push_command,
            )
            .await?;

        // This is only relevant on PRs, missing a base commit denotes that
        // this jobset creation is done for a base_commit
        if ci_check_info.base_commit.is_some() {
            let (new_jobs, changed_drvs, _removed_jobs) = self
                .db_service
                .job_difference(
                    &ci_check_info.commit,
                    &ci_check_info.base_commit.as_ref().unwrap(),
                    &name,
                )
                .await?;

            // Update the Job table to mark which jobs are new/changed
            let new_drv_ids: Vec<DrvId> = new_jobs.iter().map(|d| d.drv_path.clone()).collect();
            let changed_drv_ids: Vec<DrvId> =
                changed_drvs.iter().map(|d| d.drv_path.clone()).collect();

            self.db_service
                .update_job_differences(jobset_id, &new_drv_ids, &changed_drv_ids)
                .await?;

            // Note: We no longer create check_runs eagerly here
            // Check_runs will be created lazily when jobs fail
        }
        Ok(())
    }

    async fn handle_github_task(&mut self, task: &GitHubTask) -> Result<()> {
        use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

        match task {
            GitHubTask::UpdateBuildStatus { drv_id, status } => {
                let check_runs = self.db_service.check_runs_for_drv_path(drv_id).await?;

                for check_run in check_runs {
                    debug!("Updating checkrun status of {}", &check_run.check_run_id);
                    let octocrab = self.octocrab_for_owner(&check_run.repo_owner)?;
                    check_run.send_gh_update(&octocrab, status).await?;
                }
            },
            GitHubTask::CreateJobSet {
                ci_check_info,
                name,
                jobs,
                push_command,
            } => {
                self.create_job_set(ci_check_info, name, jobs, push_command.as_deref())
                    .await?;
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
                    .remove(&ci_check_info.commit)
                    .context("No configure gate check run found for commit")?;
                actions::update_ci_configure_gate(
                    &octocrab,
                    ci_check_info,
                    check_run_id,
                    CheckRunStatus::Completed,
                    CheckRunConclusion::Success,
                )
                .await?;
            },
            GitHubTask::CreateCIEvalJob {
                ci_check_info,
                job_title,
            } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                let check_run =
                    actions::create_ci_eval_job(&octocrab, job_title, ci_check_info).await?;
                self.github_eval_checks.insert(
                    (ci_check_info.commit.clone(), job_title.clone()),
                    check_run.id,
                );
            },
            GitHubTask::CompleteCIEvalJob {
                ci_check_info,
                job_name,
                conclusion,
            } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                let check_run_id = self
                    .github_eval_checks
                    .remove(&(ci_check_info.commit.clone(), job_name.clone()))
                    .context("No eval job check run found for commit")?;
                actions::update_ci_eval_job(
                    &octocrab,
                    ci_check_info,
                    check_run_id,
                    CheckRunStatus::Completed,
                    *conclusion,
                )
                .await?;
            },
            GitHubTask::CancelCheckRunsForCommit { ci_check_info } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

                // Cancel any in-progress configure gate
                if let Some(check_run_id) =
                    self.github_configure_checks.remove(&ci_check_info.commit)
                {
                    if let Err(e) = actions::update_ci_configure_gate(
                        &octocrab,
                        ci_check_info,
                        check_run_id,
                        CheckRunStatus::Completed,
                        CheckRunConclusion::Cancelled,
                    )
                    .await
                    {
                        warn!(
                            "Failed to cancel configure gate for {}: {:?}",
                            &ci_check_info.commit, e
                        );
                    }
                }

                // Cancel any in-progress eval gates (there may be multiple for different jobs)
                let keys_to_remove: Vec<_> = self
                    .github_eval_checks
                    .keys()
                    .filter(|(commit, _)| commit == &ci_check_info.commit)
                    .cloned()
                    .collect();

                for key in keys_to_remove {
                    if let Some(check_run_id) = self.github_eval_checks.remove(&key) {
                        if let Err(e) = actions::update_ci_eval_job(
                            &octocrab,
                            ci_check_info,
                            check_run_id,
                            CheckRunStatus::Completed,
                            CheckRunConclusion::Cancelled,
                        )
                        .await
                        {
                            warn!(
                                "Failed to cancel eval gate for {}: {:?}",
                                &ci_check_info.commit, e
                            );
                        }
                    }
                }

                // Cancel all job check_runs for this commit
                let check_runs = self
                    .db_service
                    .check_runs_for_commit(&ci_check_info.commit)
                    .await?;
                for check_run in check_runs {
                    if let Err(e) = check_run
                        .send_gh_update(
                            &octocrab,
                            &DrvBuildState::Interrupted(
                                crate::db::model::build_event::DrvBuildInterruptionKind::Cancelled,
                            ),
                        )
                        .await
                    {
                        warn!(
                            "Failed to cancel check_run {} for {}: {:?}",
                            check_run.check_run_id, &ci_check_info.commit, e
                        );
                    }
                }
            },
            GitHubTask::CreateFailureCheckRun {
                drv_id,
                jobset_id,
                job_attr_name,
                difference,
            } => {
                // Get jobset info to get commit, repo, etc.
                let jobset_info = self.db_service.get_jobset_info(*jobset_id).await?;
                let octocrab = self.octocrab_for_owner(&jobset_info.owner)?;

                // Get current build state
                let drv = self.db_service.get_drv(drv_id).await?;
                let state = drv
                    .map(|x| x.build_state)
                    .unwrap_or(DrvBuildState::Completed(
                        crate::db::model::build_event::DrvBuildResult::Failure,
                    ));

                // Create CICheckInfo
                let ci_check_info = CICheckInfo {
                    commit: jobset_info.sha.clone(),
                    base_commit: None,
                    owner: jobset_info.owner.clone(),
                    repo_name: jobset_info.repo_name.clone(),
                };

                // Create the check_run
                let check_run = ci_check_info
                    .create_gh_check_run(
                        &octocrab,
                        &jobset_info.job,
                        job_attr_name,
                        state,
                        difference,
                    )
                    .await?;

                // Store the check_run info in the database
                self.db_service
                    .insert_check_run_info(
                        check_run.id.0 as i64,
                        drv_id,
                        &jobset_info.repo_name,
                        &jobset_info.owner,
                    )
                    .await?;
            },
            GitHubTask::CreateApprovalRequiredCheckRun {
                ci_check_info,
                username,
            } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                actions::create_approval_required_check_run(&octocrab, ci_check_info, username)
                    .await?;
            },
            GitHubTask::FailCIEvalJob {
                ci_check_info,
                job_name,
                errors,
            } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                actions::fail_ci_eval_job(&octocrab, ci_check_info, job_name, errors).await?;
            },
        }
        Ok(())
    }
}
