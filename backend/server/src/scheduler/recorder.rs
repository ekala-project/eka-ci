use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::db::model::{build, build_event, drv_id};
use crate::github::GitHubTask;
use crate::scheduler::ingress::IngressTask;

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
                self.db_service
                    .update_drv_status(&drv, &task.result)
                    .await?;

                // Get all transitive referrers (downstream packages that depend on this)
                let transitive_referrers =
                    self.db_service.get_all_transitive_referrers(&drv).await?;

                if !transitive_referrers.is_empty() {
                    debug!(
                        "Marking {} transitive referrers as TransitiveFailure for {}",
                        transitive_referrers.len(),
                        drv.store_path()
                    );

                    // Record the transitive failure relationships and update build states
                    // This updates both the TransitiveFailure table and each drv's build_state
                    self.db_service
                        .insert_transitive_failures(&drv, &transitive_referrers)
                        .await?;
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
}
