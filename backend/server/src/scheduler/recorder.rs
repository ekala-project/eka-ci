use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::db::model::{build, build_event, drv_id};
use crate::github::GitHubTask;
use crate::scheduler::ingress::IngressTask;
use crate::db::model::DrvId;

#[derive(Debug, Clone)]
pub struct RecorderTask {
    pub derivation: drv_id::DrvId,
    pub result: build_event::DrvBuildState,
}

/// This services records the event of a build. Depending on the build result,
/// this service may also enqueue new ingress requests.
pub struct RecorderService {
    db_service: DbService,
    recorder_receiver: mpsc::Receiver<RecorderTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
}

/// Encapsulation of the Recorder thread. May want to gracefully recover from
/// any one particular thread going into a bad state
struct RecorderWorker {
    /// To send buildable requests to builder service
    ingress_sender: mpsc::Sender<IngressTask>,
    recorder_receiver: mpsc::Receiver<RecorderTask>,
    db_service: DbService,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
}

impl RecorderService {
    pub fn init(
        db_service: DbService,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
    ) -> (Self, mpsc::Sender<RecorderTask>) {
        let (recorder_sender, recorder_receiver) = mpsc::channel(1000);

        let res = Self {
            db_service,
            recorder_receiver,
            github_sender,
        };

        (res, recorder_sender)
    }

    pub fn run(self, ingress_sender: mpsc::Sender<IngressTask>) -> JoinHandle<()> {
        let worker = RecorderWorker::new(
            self.db_service.clone(),
            ingress_sender,
            self.recorder_receiver,
            self.github_sender,
        );

        tokio::spawn(async move {
            worker.ingest_requests().await;
        })
    }
}

impl RecorderWorker {
    fn new(
        db_service: DbService,
        ingress_sender: mpsc::Sender<IngressTask>,
        recorder_receiver: mpsc::Receiver<RecorderTask>,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
    ) -> Self {
        Self {
            db_service,
            ingress_sender,
            recorder_receiver,
            github_sender,
        }
    }

    /// Get output paths for a derivation using nix-store
    async fn get_output_paths(&self, drv_id: &drv_id::DrvId) -> anyhow::Result<Vec<String>> {
        use tokio::process::Command;

        let output = Command::new("nix-store")
            .args(["--query", "--outputs", &drv_id.store_path()])
            .output()
            .await?;

        if !output.status.success() {
            anyhow::bail!(
                "Failed to query output paths for {}: {}",
                drv_id.store_path(),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let paths = String::from_utf8(output.stdout)?
            .lines()
            .map(|s| s.to_string())
            .collect();

        Ok(paths)
    }

    /// Execute a push command with the given output paths
    async fn execute_push_command(
        &self,
        push_command: &str,
        output_paths: &[String],
        drv_id: &drv_id::DrvId,
    ) -> anyhow::Result<()> {
        use std::time::Duration;
        use tokio::process::Command;

        debug!(
            "Executing push command for {}: {}",
            drv_id.store_path(),
            push_command
        );

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(push_command)
            .env_clear() // Security: Clear all environment variables
            .env("OUT_PATHS", output_paths.join(" "))
            .env("DRV_PATH", drv_id.store_path())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("PATH", "/usr/bin:/bin:/run/current-system/sw/bin") // Minimal PATH including NixOS path
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Security: 10 minute timeout for push commands
        let timeout = Duration::from_secs(600);
        match tokio::time::timeout(timeout, command.output()).await {
            Ok(Ok(output)) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!("Push command failed: {}", stderr);
                }
                debug!("Push command completed successfully for {}", drv_id.store_path());
                Ok(())
            },
            Ok(Err(e)) => Err(e.into()),
            Err(_) => anyhow::bail!("Push command timed out after 10 minutes"),
        }
    }

    async fn ingest_requests(mut self) {
        loop {
            if let Some(task) = self.recorder_receiver.recv().await {
                debug!("Received recorder task {:?}", &task);
                if let Err(e) = self.handle_recorder_request(task.clone()).await {
                    warn!("Failed to handle ingress request {:?}: {:?}", &task, e);
                }
            }
        }
    }

    async fn handle_recorder_request(&self, task: RecorderTask) -> anyhow::Result<()> {
        use build_event::*;
        use {DrvBuildResult as DBR, DrvBuildState as DBS};

        let drv = task.derivation.clone();
        let build_id = build::DrvBuildId {
            derivation: task.derivation,
            // TODO: build_attempt seems like something we should query
            build_attempt: std::num::NonZeroU32::new(1).unwrap(),
        };

        match &task.result {
            DBS::Completed(DBR::Success) => {
                debug!(
                    "Attempting to record successful build of {}",
                    build_id.derivation.store_path()
                );
                self.db_service
                    .update_drv_status(&drv, &task.result)
                    .await?;

                // Execute push commands for all jobsets containing this drv
                let job_infos = self.db_service.get_job_info_for_drv(&drv).await?;
                for job_info in &job_infos {
                    self.handle_completed_jobset(job_info.jobset_id, &drv).await
                }


                // Clear any transitive failures caused by this drv (if it was previously failed)
                let unblocked_drvs = self.db_service.clear_transitive_failures(&drv).await?;

                // Re-queue drvs that were blocked by this failure
                for unblocked_drv in unblocked_drvs {
                    // Check if there are any other failures blocking this drv
                    let other_failures = self
                        .db_service
                        .get_failed_dependencies(&unblocked_drv)
                        .await?;

                    if other_failures.is_empty() {
                        // No other failures blocking, update state to Queued and check buildability
                        self.db_service
                            .update_drv_status(&unblocked_drv, &DBS::Queued)
                            .await?;

                        let task = IngressTask::CheckBuildable(unblocked_drv);
                        self.ingress_sender.send(task).await?;
                    }
                }

                // Check direct referrers for buildability
                let referrers = self.db_service.drv_referrers(&drv).await?;
                for referrer in referrers {
                    let task = IngressTask::CheckBuildable(referrer);
                    self.ingress_sender.send(task).await?;
                }
            },
            DBS::Completed(DBR::Failure) => {
                debug!(
                    "Attempting to record failed build of {}",
                    build_id.derivation.store_path()
                );

                // Check current state to determine if this is first or second failure
                let current_drv = self
                    .db_service
                    .get_drv(&drv)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Drv not found: {}", drv.store_path()))?;

                match current_drv.build_state {
                    DBS::Buildable => {
                        // First failure - transition to FailedRetry and re-queue immediately
                        debug!(
                            "First failure for {}, transitioning to FailedRetry",
                            drv.store_path()
                        );
                        self.db_service
                            .update_drv_status(&drv, &DBS::FailedRetry)
                            .await?;

                        // Re-queue immediately for retry
                        let task = IngressTask::CheckBuildable(drv.clone());
                        self.ingress_sender.send(task).await?;
                    },
                    DBS::FailedRetry => {
                        // Second failure - permanent failure, propagate to downstream
                        debug!(
                            "Second failure for {}, marking as permanent failure",
                            drv.store_path()
                        );
                        self.db_service
                            .update_drv_status(&drv, &task.result)
                            .await?;

                        // Get all transitive referrers and mark as TransitiveFailure
                        let transitive_referrers =
                            self.db_service.get_all_transitive_referrers(&drv).await?;
                        if !transitive_referrers.is_empty() {
                            self.db_service
                                .insert_transitive_failures(&drv, &transitive_referrers)
                                .await?;
                        }
                    },
                    _ => {
                        // Unexpected state - log warning but still record failure
                        warn!(
                            "Unexpected state {:?} when recording failure for {}",
                            current_drv.build_state,
                            drv.store_path()
                        );
                        self.db_service
                            .update_drv_status(&drv, &task.result)
                            .await?;
                    },
                }
            },
            _ => {},
        }

        if let Some(github_sender) = &self.github_sender {
            // Check if we need to create check_runs for failures
            // Only create check_runs if the build failed and no check_run exists yet
            if task.result.is_failure() {
                let existing_check_runs = self.db_service.check_runs_for_drv_path(&drv).await?;

                if existing_check_runs.is_empty() {
                    // No check_run exists, we need to create one
                    // Get job info to know which jobsets this drv belongs to
                    let job_infos = self.db_service.get_job_info_for_drv(&drv).await?;

                    for job_info in job_infos {
                        let create_task = GitHubTask::CreateFailureCheckRun {
                            drv_id: drv.clone(),
                            jobset_id: job_info.jobset_id,
                            job_attr_name: job_info.name.clone(),
                            difference: job_info.difference,
                        };
                        if let Err(e) = github_sender.send(create_task).await {
                            warn!(
                                "Failed to send CreateFailureCheckRun for {}: {:?}",
                                drv.store_path(),
                                e
                            );
                        }
                    }
                }
            }

            // Send update for existing check_runs
            let github_task = GitHubTask::UpdateBuildStatus {
                drv_id: drv.clone(),
                status: task.result.clone(),
            };
            if let Err(e) = github_sender.send(github_task).await {
                warn!(
                    "Failed to send GitHub update for {}: {:?}",
                    drv.store_path(),
                    e
                );
            }

            // Check if this drv completion concludes any jobsets
            // Only check if we've reached a terminal state
            if task.result.is_terminal() {
                let job_infos = self.db_service.get_job_info_for_drv(&drv).await?;

                for job_info in job_infos {
                    // Check if all jobs in this jobset are concluded
                    if self
                        .db_service
                        .all_jobs_concluded(job_info.jobset_id)
                        .await?
                    {
                        // Determine conclusion based on new/changed job failures
                        let has_failures = self
                            .db_service
                            .jobset_has_new_or_changed_failures(job_info.jobset_id)
                            .await?;

                        let conclusion = if has_failures {
                            octocrab::params::checks::CheckRunConclusion::Failure
                        } else {
                            octocrab::params::checks::CheckRunConclusion::Success
                        };

                        // Get jobset info to get the job name
                        let jobset_info =
                            self.db_service.get_jobset_info(job_info.jobset_id).await?;

                        let complete_task = GitHubTask::CompleteCIEvalJob {
                            ci_check_info: crate::github::CICheckInfo {
                                commit: jobset_info.sha.clone(),
                                base_commit: None,
                                owner: jobset_info.owner.clone(),
                                repo_name: jobset_info.repo_name.clone(),
                            },
                            job_name: jobset_info.job.clone(),
                            conclusion,
                        };

                        if let Err(e) = github_sender.send(complete_task).await {
                            warn!(
                                "Failed to send CompleteCIEvalJob for jobset {}: {:?}",
                                job_info.jobset_id, e
                            );
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_completed_jobset(&self, jobset_id: i64, drv: &DrvId) {
        match self.db_service.get_jobset_info(jobset_id).await {
            Ok(jobset_info) => {
                if let Some(push_cmd) = &jobset_info.push_command {
                    // Get output paths
                    match self.get_output_paths(&drv).await {
                        Ok(output_paths) => {
                            // Execute push command (warn on failure, don't fail build)
                            if let Err(e) =
                                self.execute_push_command(push_cmd, &output_paths, &drv)
                                    .await
                            {
                                warn!(
                                    "Push command failed for {} in jobset {}: {:?}",
                                    drv.store_path(),
                                    jobset_id,
                                    e
                                );
                            }
                        },
                        Err(e) => {
                            warn!(
                                "Failed to get output paths for {}: {:?}",
                                drv.store_path(),
                                e
                            );
                        },
                    }
                }
            },
            Err(e) => {
                warn!(
                    "Failed to get jobset info for jobset {}: {:?}",
                    jobset_id, e
                );
            },
        }
    }

}
