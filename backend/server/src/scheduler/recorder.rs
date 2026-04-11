use anyhow::Context as _;
use chrono::Utc;
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::ci::config::Job;
use crate::db::DbService;
use crate::db::model::{build, build_event, drv_id};
use crate::github::GitHubTask;
use crate::graph::GraphCommand;
use crate::hooks::types::{HookContext, HookTask};
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
    hook_sender: Option<mpsc::Sender<HookTask>>,
    cache_configs: std::sync::Arc<std::collections::HashMap<String, crate::config::CacheConfig>>,
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
    hook_sender: Option<mpsc::Sender<HookTask>>,
    cache_configs: std::sync::Arc<std::collections::HashMap<String, crate::config::CacheConfig>>,
}

impl RecorderService {
    pub fn init(
        db_service: DbService,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        websocket_sender: Option<broadcast::Sender<ServerEvent>>,
        graph_command_sender: mpsc::Sender<GraphCommand>,
        hook_sender: Option<mpsc::Sender<HookTask>>,
        cache_configs: std::sync::Arc<
            std::collections::HashMap<String, crate::config::CacheConfig>,
        >,
    ) -> (Self, mpsc::Sender<RecorderTask>) {
        let (recorder_sender, recorder_receiver) = mpsc::channel(1000);

        let res = Self {
            db_service,
            recorder_receiver,
            github_sender,
            websocket_sender,
            graph_command_sender,
            hook_sender,
            cache_configs,
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
            self.hook_sender,
            self.cache_configs,
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
        hook_sender: Option<mpsc::Sender<HookTask>>,
        cache_configs: std::sync::Arc<
            std::collections::HashMap<String, crate::config::CacheConfig>,
        >,
    ) -> Self {
        Self {
            db_service,
            ingress_sender,
            recorder_receiver,
            github_sender,
            websocket_sender,
            graph_command_sender,
            hook_sender,
            cache_configs,
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
    async fn clear_graph_failure(
        &self,
        drv_id: &drv_id::DrvId,
    ) -> anyhow::Result<Vec<drv_id::DrvId>> {
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
    async fn propagate_graph_failure(
        &self,
        drv_id: &drv_id::DrvId,
    ) -> anyhow::Result<Vec<drv_id::DrvId>> {
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

                // Execute post-build hooks if configured
                if let Err(e) = self.execute_hooks_for_drv(&drv).await {
                    warn!("Failed to execute hooks for {}: {}", drv.store_path(), e);
                    // Don't fail the build if hooks fail - they run asynchronously
                }

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

    /// Build a post-build hook command for pushing to a cache
    async fn build_cache_push_hook(
        cache_config: &crate::config::CacheConfig,
    ) -> anyhow::Result<crate::hooks::types::PostBuildHook> {
        use crate::config::CacheType;
        use crate::hooks::types::PostBuildHook;

        // Load credentials from configured source
        let credentials = cache_config.credentials.load().await.with_context(|| {
            format!("Failed to load credentials for cache '{}'", cache_config.id)
        })?;

        debug!(
            "Loaded credentials for cache '{}' from {:?}",
            cache_config.id, cache_config.credentials
        );

        // Build command based on cache type
        let command = match cache_config.cache_type {
            CacheType::NixCopy => {
                vec![
                    "nix".to_string(),
                    "copy".to_string(),
                    "--to".to_string(),
                    cache_config.destination.clone(),
                    "$OUT_PATHS".to_string(), // Will be expanded by HookExecutor
                ]
            },
            CacheType::Cachix => {
                vec![
                    "cachix".to_string(),
                    "push".to_string(),
                    cache_config.destination.clone(),
                    "$OUT_PATHS".to_string(),
                ]
            },
            CacheType::Attic => {
                vec![
                    "attic".to_string(),
                    "push".to_string(),
                    cache_config.destination.clone(),
                    "$OUT_PATHS".to_string(),
                ]
            },
        };

        Ok(PostBuildHook {
            name: format!("push-{}", cache_config.id),
            command,
            env: credentials,
        })
    }

    /// Query the output paths of a derivation using nix-store
    async fn get_drv_output_paths(drv_path: &str) -> anyhow::Result<Vec<String>> {
        debug!("Querying output paths for drv: {}", drv_path);

        let output = Command::new("nix-store")
            .args(["--query", "--outputs", drv_path])
            .output()
            .await
            .with_context(|| format!("Failed to execute nix-store for drv: {}", drv_path))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("nix-store query failed for {}: {}", drv_path, stderr);
        }

        let paths: Vec<String> = String::from_utf8(output.stdout)
            .context("Invalid UTF-8 in nix-store output")?
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if paths.is_empty() {
            warn!("No output paths found for drv: {}", drv_path);
        } else {
            debug!("Found {} output path(s) for drv: {}", paths.len(), drv_path);
        }

        Ok(paths)
    }

    /// Execute post-build hooks for a successfully built drv
    /// This resolves cache references from job config and checks permissions
    async fn execute_hooks_for_drv(&self, drv_id: &drv_id::DrvId) -> anyhow::Result<()> {
        use crate::cache_permissions::{PermissionContext, check_cache_permission};

        // Return early if no hook sender configured
        let hook_sender: &mpsc::Sender<HookTask> = match &self.hook_sender {
            Some(sender) => sender,
            None => return Ok(()), // No hooks configured
        };

        // Get the job config from the database
        let config_json = match self.db_service.get_job_config_for_drv(drv_id).await? {
            Some(json) => json,
            None => return Ok(()), // No config stored for this drv
        };

        // Parse the job config
        let job: Job = serde_json::from_str(&config_json)?;

        // If no caches configured, return early
        if job.caches.is_empty() {
            return Ok(());
        }

        // Get drv info for hook context
        let drv_info = self
            .db_service
            .get_drv(drv_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Drv not found: {}", drv_id.store_path()))?;

        // Get job info for context (job name, commit sha, etc.)
        let job_infos = self.db_service.get_job_info_for_drv(drv_id).await?;
        let job_info = job_infos
            .first()
            .ok_or_else(|| anyhow::anyhow!("No job info found for drv"))?;

        // Get jobset info to extract commit SHA and repo details
        let jobset_info = self
            .db_service
            .get_jobset_by_id(job_info.jobset_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Jobset not found"))?;

        // Build permission context from jobset info
        let permission_context = PermissionContext {
            repo_owner: jobset_info.owner.clone(),
            repo_name: jobset_info.repo_name.clone(),
            branch: None, // TODO: Extract branch info from jobset if available
        };

        // Resolve cache IDs to cache configs and check permissions
        let mut hooks = Vec::new();
        for cache_id in &job.caches {
            // Look up cache config from server registry
            let cache_config = match self.cache_configs.get(cache_id) {
                Some(config) => config,
                None => {
                    warn!(
                        "Cache ID '{}' not found in server registry, skipping",
                        cache_id
                    );
                    continue;
                },
            };

            // Check if this repo/branch is allowed to use this cache
            if let Err(e) = check_cache_permission(cache_config, &permission_context) {
                warn!(
                    "Permission denied for cache '{}' in {}/{}: {}",
                    cache_id, permission_context.repo_owner, permission_context.repo_name, e
                );
                continue;
            }

            // Build actual hook command from cache config
            match Self::build_cache_push_hook(cache_config).await {
                Ok(hook) => {
                    debug!("Created cache push hook for cache '{}'", cache_id);
                    hooks.push(hook);
                },
                Err(e) => {
                    warn!(
                        "Failed to build cache push hook for '{}': {}. Skipping this cache.",
                        cache_id, e
                    );
                    // Continue to next cache instead of failing the entire hook execution
                    continue;
                },
            }
        }

        // If no hooks to execute after filtering, return early
        if hooks.is_empty() {
            return Ok(());
        }

        // Build hook context
        let context = HookContext {
            job_name: job_info.name.clone(),
            is_fod: drv_info.is_fod,
            system: drv_info.system.clone(),
            pname: None, // TODO: Query pname from DrvInfo if needed
            build_log_path: format!("logs/{}/build.log", drv_id.store_path()), /* TODO: Use actual log path */
            commit_sha: jobset_info.sha.clone(),
        };

        // Query the actual output paths from nix-store
        let out_paths = match Self::get_drv_output_paths(&drv_id.store_path()).await {
            Ok(paths) if !paths.is_empty() => paths,
            Ok(_) => {
                warn!(
                    "No output paths found for drv {}, using drv path as fallback",
                    drv_id.store_path()
                );
                vec![drv_id.store_path().to_string()]
            },
            Err(e) => {
                warn!(
                    "Failed to query output paths for drv {}: {}. Using drv path as fallback.",
                    drv_id.store_path(),
                    e
                );
                vec![drv_id.store_path().to_string()]
            },
        };

        // Create and send the hook task
        let hook_task = HookTask {
            drv_path: drv_id.store_path().to_string(),
            out_paths,
            hooks,
            context,
        };

        hook_sender.send(hook_task).await?;

        debug!(
            "Sent hook task for drv {} (job: {})",
            drv_id.store_path(),
            job_info.name
        );

        Ok(())
    }
}
