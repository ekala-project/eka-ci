use chrono::Utc;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::db::model::{build, build_event, drv_id};
use crate::github::GitHubTask;
use crate::graph::GraphCommand;
use crate::scheduler::ingress::IngressTask;
use crate::services::websocket::events::{BuildStateChange, JobStatsUpdate, ServerEvent};

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
    websocket_sender: Option<broadcast::Sender<ServerEvent>>,
    graph_command_sender: mpsc::Sender<GraphCommand>,
}

/// Encapsulation of the Recorder thread. May want to gracefully recover from
/// any one particular thread going into a bad state
struct RecorderWorker {
    /// To send buildable requests to builder service
    ingress_sender: mpsc::Sender<IngressTask>,
    recorder_receiver: mpsc::Receiver<RecorderTask>,
    db_service: DbService,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    websocket_sender: Option<broadcast::Sender<ServerEvent>>,
    graph_command_sender: mpsc::Sender<GraphCommand>,
}

impl RecorderService {
    pub fn init(
        db_service: DbService,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        websocket_sender: Option<broadcast::Sender<ServerEvent>>,
        graph_command_sender: mpsc::Sender<GraphCommand>,
    ) -> (Self, mpsc::Sender<RecorderTask>) {
        let (recorder_sender, recorder_receiver) = mpsc::channel(1000);

        let res = Self {
            db_service,
            recorder_receiver,
            github_sender,
            websocket_sender,
            graph_command_sender,
        };

        (res, recorder_sender)
    }

    pub fn run(self, ingress_sender: mpsc::Sender<IngressTask>) -> JoinHandle<()> {
        let worker = RecorderWorker::new(
            self.db_service.clone(),
            ingress_sender,
            self.recorder_receiver,
            self.github_sender,
            self.websocket_sender,
            self.graph_command_sender,
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
        websocket_sender: Option<broadcast::Sender<ServerEvent>>,
        graph_command_sender: mpsc::Sender<GraphCommand>,
    ) -> Self {
        Self {
            db_service,
            ingress_sender,
            recorder_receiver,
            github_sender,
            websocket_sender,
            graph_command_sender,
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

    /// Broadcast a build state change event to WebSocket clients
    fn broadcast_state_change(
        &self,
        drv: &drv_id::DrvId,
        old_state: &build_event::DrvBuildState,
        new_state: &build_event::DrvBuildState,
    ) {
        if let Some(ref sender) = self.websocket_sender {
            let event = ServerEvent::BuildStateChange(BuildStateChange {
                drv_path: drv.store_path().to_string(),
                old_state: old_state.clone(),
                new_state: new_state.clone(),
                timestamp: Utc::now(),
            });

            // Broadcast the event (ignore if no receivers)
            let _ = sender.send(event);
        }
    }

    /// Broadcast job statistics update for a specific job
    async fn broadcast_job_stats(&self, jobset_id: i64) {
        if let Some(ref sender) = self.websocket_sender {
            // Fetch current job stats from database
            if let Ok(details) = self.db_service.get_jobset_details(jobset_id).await {
                let event = ServerEvent::JobStatsUpdate(JobStatsUpdate {
                    jobset_id,
                    total_drvs: details.total_drvs,
                    queued_drvs: details.queued_drvs,
                    buildable_drvs: details.buildable_drvs,
                    building_drvs: details.building_drvs,
                    completed_success_drvs: details.completed_success_drvs,
                    completed_failure_drvs: details.completed_failure_drvs,
                    failed_retry_drvs: details.failed_retry_drvs,
                    transitive_failure_drvs: details.transitive_failure_drvs,
                    blocked_drvs: details.blocked_drvs,
                    interrupted_drvs: details.interrupted_drvs,
                    timestamp: Utc::now(),
                });

                // Broadcast the event (ignore if no receivers)
                let _ = sender.send(event);
            }
        }
    }

    /// Update drv status in both graph and database, then broadcast the change
    async fn update_and_broadcast(
        &self,
        drv: &drv_id::DrvId,
        old_state: &build_event::DrvBuildState,
        new_state: &build_event::DrvBuildState,
    ) -> anyhow::Result<()> {
        // Update graph first (fast in-memory operation)
        self.update_graph_state(drv, new_state.clone()).await?;

        // Then update database (for persistence)
        self.db_service.update_drv_status(drv, new_state).await?;

        // Finally broadcast to websocket clients
        self.broadcast_state_change(drv, old_state, new_state);
        Ok(())
    }

    /// Send UpdateState command to graph service
    async fn update_graph_state(
        &self,
        drv_id: &drv_id::DrvId,
        new_state: build_event::DrvBuildState,
    ) -> anyhow::Result<()> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = GraphCommand::UpdateState {
            drv_id: drv_id.clone(),
            new_state,
            response: tx,
        };

        self.graph_command_sender.send(cmd).await?;
        rx.await??;
        Ok(())
    }

    /// Send ClearFailure command to graph service
    async fn clear_graph_failure(&self, drv_id: &drv_id::DrvId) -> anyhow::Result<Vec<drv_id::DrvId>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = GraphCommand::ClearFailure {
            formerly_failed: drv_id.clone(),
            response: tx,
        };

        self.graph_command_sender.send(cmd).await?;
        let unblocked = rx.await??;
        Ok(unblocked)
    }

    /// Send PropagateFailure command to graph service
    async fn propagate_graph_failure(&self, drv_id: &drv_id::DrvId) -> anyhow::Result<Vec<drv_id::DrvId>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let cmd = GraphCommand::PropagateFailure {
            failed_drv: drv_id.clone(),
            response: tx,
        };

        self.graph_command_sender.send(cmd).await?;
        let blocked = rx.await??;
        Ok(blocked)
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

        // Get job info to broadcast stats updates later
        let job_infos = self.db_service.get_job_info_for_drv(&drv).await?;

        match &task.result {
            DBS::Completed(DBR::Success) => {
                debug!(
                    "Attempting to record successful build of {}",
                    build_id.derivation.store_path()
                );
                // Get old state before updating
                let old_state = self
                    .db_service
                    .get_drv(&drv)
                    .await?
                    .map(|d| d.build_state)
                    .unwrap_or(DBS::Queued);

                self.update_and_broadcast(&drv, &old_state, &task.result)
                    .await?;

                // Clear any transitive failures in graph (fast in-memory operation)
                let unblocked_drvs = self.clear_graph_failure(&drv).await?;

                // Also clear in database for persistence
                self.db_service.clear_transitive_failures(&drv).await?;

                // Re-queue drvs that were unblocked
                for unblocked_drv in unblocked_drvs {
                    let task = IngressTask::CheckBuildable(unblocked_drv);
                    self.ingress_sender.send(task).await?;
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
                        let old_state = current_drv.build_state.clone();
                        self.update_and_broadcast(&drv, &old_state, &DBS::FailedRetry)
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
                        let old_state = current_drv.build_state.clone();
                        self.update_and_broadcast(&drv, &old_state, &task.result)
                            .await?;

                        // Propagate failure in graph (fast in-memory BFS traversal)
                        let blocked_drvs = self.propagate_graph_failure(&drv).await?;

                        // Also propagate in database for persistence
                        if !blocked_drvs.is_empty() {
                            self.db_service
                                .insert_transitive_failures(&drv, &blocked_drvs)
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
                        let old_state = current_drv.build_state.clone();
                        self.update_and_broadcast(&drv, &old_state, &task.result)
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

        // Broadcast job stats updates for all affected jobs
        for job_info in job_infos {
            self.broadcast_job_stats(job_info.jobset_id).await;
        }

        Ok(())
    }
}
