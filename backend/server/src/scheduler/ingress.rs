use std::sync::Arc;

use anyhow::Context;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};
use crate::db::model::drv_id;
use crate::graph::GraphServiceHandle;
use crate::graph_compat;
use crate::scheduler::build::{BuildRequest, BuilderFeatureSnapshot};
use crate::scheduler::recorder::RecorderTask;
use crate::services::TaskJournal;

/// This acts as the service which filters incoming drv build requests
/// and determines if the drv is "buildable", already successful,
/// already failed, has a dependency failure, otherwise it will mark it as queued.
pub struct IngressService {
    graph_handle: GraphServiceHandle,
    request_receiver: mpsc::Receiver<IngressTask>,
}

pub struct IngressWorker {
    /// To receive requests for updating or inserting drvs
    request_receiver: mpsc::Receiver<IngressTask>,
    /// To send buildable requests to builder service
    buildable_sender: mpsc::Sender<BuildRequest>,
    /// To short-circuit cache hits straight to "successful build" without
    /// going through the builder thread.
    recorder_sender: mpsc::Sender<RecorderTask>,
    graph_handle: GraphServiceHandle,
    journal: TaskJournal<IngressTask>,
    builder_features: BuilderFeatureSnapshot,
}

/// Variants carry `Arc<DrvId>` so fan-out senders (recorder, webhooks,
/// nix eval) can `Arc::clone` instead of cloning the inner `String`
/// when the same drv crosses multiple channel hops.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum IngressTask {
    /// This is a Drv which was determined by an evaluation
    /// The actual status is unknown. Could be new, or could have already completed.
    EvalRequest(Arc<drv_id::DrvId>),
    /// This is a Drv which we can safely assume had already added and
    /// a dependency was successfully built, and now we should recheck to
    /// to see if the Drv is now buildable
    CheckBuildable(Arc<drv_id::DrvId>),
    /// Re-run the substitution check for this drv. Normally fired
    /// implicitly during `EvalRequest` handling; exposed as a task so
    /// operators / tests can re-trigger it explicitly (e.g. after a
    /// remote cache becomes reachable).
    ///
    /// Not yet constructed in production code — kept on the public
    /// task surface for forthcoming admin endpoints. Silence the
    /// dead-code lint until a caller lands.
    #[allow(dead_code)]
    CheckSubstitution(Arc<drv_id::DrvId>),
    /// Rebuild a failed drv by resetting it to Queued and clearing failure tracking
    RebuildFailed(Arc<drv_id::DrvId>),
    /// Rebuild all failed drvs in the system
    RebuildAllFailed,
}

impl IngressService {
    pub fn init(graph_handle: GraphServiceHandle) -> (Self, mpsc::Sender<IngressTask>) {
        let (request_sender, request_receiver) = mpsc::channel(50_000);

        let res = Self {
            graph_handle,
            request_receiver,
        };

        (res, request_sender)
    }

    pub fn run(
        self,
        buildable_sender: mpsc::Sender<BuildRequest>,
        recorder_sender: mpsc::Sender<RecorderTask>,
        cancellation_token: CancellationToken,
        pool: sqlx::SqlitePool,
        builder_features: BuilderFeatureSnapshot,
    ) -> JoinHandle<()> {
        let worker = IngressWorker {
            request_receiver: self.request_receiver,
            buildable_sender,
            recorder_sender,
            graph_handle: self.graph_handle,
            journal: TaskJournal::new(pool, "ingress"),
            builder_features,
        };
        tokio::spawn(async move {
            worker.ingest_requests(cancellation_token).await;
        })
    }
}

impl IngressWorker {
    async fn ingest_requests(mut self, cancellation_token: CancellationToken) {
        // Replay un-acknowledged tasks from a previous crash.
        match self.journal.recover().await {
            Ok(recovered) => {
                for (jid, task) in recovered {
                    info!("replaying recovered ingress task: {:?}", &task);
                    if let Err(e) = self.handle_ingress_request(&task).await {
                        warn!(
                            "Failed to handle recovered ingress request {:?}: {:?}",
                            &task, e
                        );
                    } else if let Err(e) = self.journal.acknowledge(jid).await {
                        warn!("failed to ack recovered ingress journal entry: {:?}", e);
                    }
                }
            },
            Err(e) => warn!("failed to recover ingress journal: {:?}", e),
        }

        // Periodic sweep interval to catch drvs whose CheckBuildable
        // messages were dropped by try_send (see todo-spec-items #4, #7).
        let mut sweep_interval = tokio::time::interval(std::time::Duration::from_secs(60));
        // Don't pile up ticks while we're busy processing tasks.
        sweep_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Skip the immediate first tick.
        sweep_interval.tick().await;

        loop {
            tokio::select! {
                request = self.request_receiver.recv() => {
                    let task = match request {
                        Some(task) => task,
                        None => {
                            warn!("Ingress receiver channel closed, shutting down");
                            break;
                        },
                    };

                    let jid = match self.journal.persist(&task).await {
                        Ok(id) => Some(id),
                        Err(e) => {
                            warn!("failed to journal ingress task {:?}: {:?}", &task, e);
                            None
                        },
                    };

                    if let Err(e) = self.handle_ingress_request(&task).await {
                        warn!("Failed to handle ingress request {:?}: {:?}", &task, e);
                    } else if let Some(id) = jid {
                        if let Err(e) = self.journal.acknowledge(id).await {
                            warn!("failed to ack ingress journal entry: {:?}", e);
                        }
                    }
                },
                _ = sweep_interval.tick() => {
                    self.sweep_buildable_drvs().await;
                },
                _ = cancellation_token.cancelled() => {
                    break;
                },
            }
        }

        info!("IngressWorker service shutdown gracefully");
    }

    /// Periodic sweep: find Queued/Buildable drvs whose deps are all
    /// Completed(Success) and re-send them to the builder. Catches drvs
    /// that were deferred because the builder channel was full, or whose
    /// CheckBuildable messages were dropped.
    async fn sweep_buildable_drvs(&self) {
        let mut swept = 0u32;
        let shared_buildable =
            crate::db::graph_impl::convert_build_state(&DrvBuildState::Buildable);
        let candidates: Vec<_> = self
            .graph_handle
            .shared_view()
            .iter()
            .filter(|e| {
                let state = &e.value().build_state;
                // Sweep both Queued (deps just completed) and Buildable
                // (deferred because builder channel was full)
                (*state == shared::types::DrvBuildState::Queued
                    && self.graph_handle.is_buildable(e.key()))
                    || *state == shared_buildable
            })
            .map(|e| e.key().clone())
            .collect();

        for shared_id in candidates {
            if let Ok(server_id) = graph_compat::to_server_drv_id(&shared_id) {
                match self.handle_check_buildable_task(&server_id).await {
                    Ok(()) => swept += 1,
                    Err(e) => warn!("sweep: {}: {:?}", server_id.store_path(), e),
                }
            }
        }
        if swept > 0 {
            info!("buildability sweep re-queued {} stuck drvs", swept);
        }
    }

    async fn handle_ingress_request(&self, task: &IngressTask) -> anyhow::Result<()> {
        use IngressTask::*;

        match task {
            EvalRequest(drv) => self.handle_eval_task(drv).await?,
            CheckBuildable(drv) => self.handle_check_buildable_task(drv).await?,
            CheckSubstitution(drv) => {
                // Standalone re-check: if cached, short-circuit; otherwise no-op.
                // Errors here are non-fatal (substitution check is best-effort).
                if let Err(e) = self.handle_check_substitution_task(drv).await {
                    warn!(
                        "substitution check failed for {}: {:?}",
                        drv.store_path(),
                        e
                    );
                }
            },
            RebuildFailed(drv) => self.handle_rebuild_failed_task(drv).await?,
            RebuildAllFailed => self.handle_rebuild_all_failed_task().await?,
        }

        // `drv` above is `&Arc<DrvId>`; handlers accept `&DrvId` via Arc deref coercion.

        Ok(())
    }

    async fn handle_check_buildable_task(&self, drv_id: &drv_id::DrvId) -> anyhow::Result<()> {
        let shared_id = graph_compat::to_shared_drv_id(drv_id)?;
        if self.graph_handle.is_buildable(&shared_id) {
            let cached_node = self
                .graph_handle
                .get_node(&shared_id)
                .context("drv is missing from graph")?;

            // Early rejection: if no builder can handle this drv's required
            // system features, mark as UnsatisfiableRequirements immediately
            // instead of sending it through the build queue.
            if !self
                .builder_features
                .can_build(&cached_node.required_system_features)
            {
                warn!(
                    "{:?} requires features {:?} that no builder provides",
                    drv_id, cached_node.required_system_features
                );
                let task = RecorderTask {
                    derivation: Arc::new(drv_id.clone()),
                    result: DrvBuildState::UnsatisfiableRequirements,
                };
                self.recorder_sender.send(task).await?;
                return Ok(());
            }

            // FailedRetry must be preserved so the recorder can detect second failures.
            let shared_failed_retry =
                crate::db::graph_impl::convert_build_state(&DrvBuildState::FailedRetry);
            if cached_node.build_state != shared_failed_retry {
                let shared_buildable =
                    crate::db::graph_impl::convert_build_state(&DrvBuildState::Buildable);
                self.graph_handle
                    .update_state(&shared_id, shared_buildable)
                    .await?;
            }

            let shared_drv = cached_node.to_drv();
            let server_drv = graph_compat::to_server_drv(&shared_drv)?;
            // Use try_send to avoid blocking the ingress when the
            // builder channel is full. The periodic sweep will
            // re-discover Buildable drvs that couldn't be sent.
            if let Err(e) = self.buildable_sender.try_send(BuildRequest(server_drv)) {
                debug!(
                    "builder channel full, deferring build for {}: {}",
                    drv_id.store_path(),
                    e
                );
            }
        }

        Ok(())
    }

    /// This attempts to update the status of a drv by inspecting the
    /// status of the dependencies.
    async fn handle_eval_task(&self, drv_id: &drv_id::DrvId) -> anyhow::Result<()> {
        debug!(
            "IngressService handling EvalRequest for: {}",
            drv_id.store_path()
        );

        let shared_id = graph_compat::to_shared_drv_id(drv_id)?;
        if let Some(build_state) = self.graph_handle.get_build_state(&shared_id) {
            if build_state.is_terminal() {
                debug!(
                    "{:?} is already in terminal state {:?}, skipping build",
                    drv_id, build_state
                );
                return Ok(());
            }
        }

        // Check if the drv is cached or if all its deps are available.
        let report = match crate::nix::dry_run_realise(drv_id).await {
            Ok(report) => report,
            Err(e) => {
                warn!(
                    "substitution check errored for {} (falling back to graph): {:?}",
                    drv_id.store_path(),
                    e
                );
                self.handle_check_buildable_task(drv_id).await?;
                return Ok(());
            },
        };

        if report.is_cached(drv_id) {
            debug!("substitution hit for {}", drv_id.store_path());
            self.handle_check_substitution_task_with_report(drv_id, &report)
                .await?;
            return Ok(());
        }

        // Not fully cached. Check if all deps are available (only
        // this drv needs building). Trust nix's dry-run assessment
        // rather than the in-memory graph (which may be incomplete).
        if report.will_build.len() == 1 && report.will_build.contains(&drv_id.store_path()) {
            debug!(
                "{} needs building but all deps available, sending to builder",
                drv_id.store_path()
            );

            // Send directly to builder — bypass graph buildability
            // check since nix confirmed all deps are available.
            let shared_id = graph_compat::to_shared_drv_id(drv_id)?;
            if let Some(cached_node) = self.graph_handle.get_node(&shared_id) {
                let shared_buildable =
                    crate::db::graph_impl::convert_build_state(&DrvBuildState::Buildable);
                if let Some(mut entry) = self.graph_handle.shared_view().get_mut(&shared_id) {
                    entry.build_state = shared_buildable;
                }
                let shared_drv = cached_node.to_drv();
                let server_drv = graph_compat::to_server_drv(&shared_drv)?;
                self.buildable_sender.send(BuildRequest(server_drv)).await?;
            } else {
                // Drv not in graph — fall back to graph-based check
                self.handle_check_buildable_task(drv_id).await?;
            }
        } else {
            debug!(
                "{} needs {} drvs built, checking graph buildability",
                drv_id.store_path(),
                report.will_build.len()
            );
            self.handle_check_buildable_task(drv_id).await?;
        }

        Ok(())
    }

    /// Run `nix-store --realise --dry-run` against `drv_id`. If the drv is
    /// already available (locally or via any configured substituter), mark
    /// it AND every transitively-cached requisite as `Completed(Success)`
    /// via the recorder.
    ///
    /// Returns `Ok(true)` iff the root drv was a cache hit (and the caller
    /// should skip queuing a real build).
    async fn handle_check_substitution_task(&self, drv_id: &drv_id::DrvId) -> anyhow::Result<bool> {
        let report = crate::nix::dry_run_realise(drv_id).await?;
        if !report.is_cached(drv_id) {
            return Ok(false);
        }
        self.handle_check_substitution_task_with_report(drv_id, &report)
            .await?;
        Ok(true)
    }

    /// Mark cached deps as Completed using a pre-computed dry-run report.
    async fn handle_check_substitution_task_with_report(
        &self,
        drv_id: &drv_id::DrvId,
        report: &crate::nix::DryRunReport,
    ) -> anyhow::Result<()> {
        // Mark the root + all transitive cached deps as Completed.
        // BFS through the in-memory graph (populated by batch_traverse)
        // instead of spawning nix-store per drv.
        let shared_root = graph_compat::to_shared_drv_id(drv_id)?;
        let mut queue = std::collections::VecDeque::new();
        let mut visited = std::collections::HashSet::new();
        queue.push_back(shared_root);

        while let Some(shared_id) = queue.pop_front() {
            if !visited.insert(shared_id.clone()) {
                continue;
            }

            // Skip if this drv needs building (not cached)
            if report.will_build.contains(&shared_id.store_path()) {
                continue;
            }

            // Skip if already terminal
            if let Some(state) = self.graph_handle.get_build_state(&shared_id) {
                let server_state = crate::db::graph_impl::convert_build_state_back(&state);
                if server_state.is_terminal() || matches!(server_state, DrvBuildState::Building) {
                    continue;
                }
            }

            // Mark as cached
            if let Ok(server_id) = graph_compat::to_server_drv_id(&shared_id) {
                let task = RecorderTask {
                    derivation: Arc::new(server_id),
                    result: DrvBuildState::Completed(DrvBuildResult::Success),
                };
                self.recorder_sender
                    .send(task)
                    .await
                    .context("recorder channel closed while recording cache hit")?;
            }

            // Enqueue direct deps for BFS
            if let Some(node) = self.graph_handle.get_node(&shared_id) {
                for dep_id in node.dependencies.iter() {
                    if !visited.contains(dep_id) {
                        queue.push_back(dep_id.clone());
                    }
                }
            }
        }

        debug!(
            "substitution BFS for {}: visited {} drvs",
            drv_id.store_path(),
            visited.len()
        );
        Ok(())
    }

    /// Rebuild a failed drv by resetting it to Queued and clearing failure tracking.
    /// Also recursively rebuilds any failed dependencies.
    async fn handle_rebuild_failed_task(&self, drv_id: &drv_id::DrvId) -> anyhow::Result<()> {
        use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};

        debug!("Attempting to rebuild failed drv: {:?}", drv_id);

        let shared_id = graph_compat::to_shared_drv_id(drv_id)?;
        let node = self
            .graph_handle
            .get_node(&shared_id)
            .context("drv not found")?;

        // Convert shared build state back to server type for matching
        let server_build_state = crate::db::graph_impl::convert_build_state_back(&node.build_state);

        match &server_build_state {
            DrvBuildState::Completed(DrvBuildResult::Failure)
            | DrvBuildState::TransitiveFailure
            | DrvBuildState::FailedRetry => {
                debug!(
                    "{:?} is in failed state {:?}, rebuilding",
                    drv_id, &server_build_state
                );

                let failed_deps = self
                    .graph_handle
                    .get_failed_dependencies(&shared_id)
                    .await?;
                for dep in &failed_deps {
                    // Convert shared dep back to server type for recursive call
                    let server_dep = graph_compat::to_server_drv_id(dep)?;
                    debug!(
                        "{:?} has failed dependency {:?}, rebuilding it first",
                        drv_id, server_dep
                    );
                    Box::pin(self.handle_rebuild_failed_task(&server_dep)).await?;
                }

                let shared_queued =
                    crate::db::graph_impl::convert_build_state(&DrvBuildState::Queued);
                self.graph_handle
                    .update_state(&shared_id, shared_queued)
                    .await?;

                // Transitive-failure cleanup happens in the graph service's
                // ClearFailure command when the drv succeeds.

                self.handle_check_buildable_task(drv_id).await?;

                debug!("{:?} has been reset and re-queued", drv_id);
            },
            _ => {
                warn!(
                    "{:?} is not in a failed state (current state: {:?}), cannot rebuild",
                    drv_id, &server_build_state
                );
            },
        }

        Ok(())
    }

    /// Rebuild all failed drvs in the system
    async fn handle_rebuild_all_failed_task(&self) -> anyhow::Result<()> {
        debug!("Rebuilding all failed drvs");

        let failed_drvs = self.graph_handle.get_all_failed_drvs().await?;
        debug!("Found {} failed drvs to rebuild", failed_drvs.len());

        for shared_drv_id in &failed_drvs {
            // Convert shared type back to server type for handle_rebuild_failed_task
            let server_drv_id = graph_compat::to_server_drv_id(shared_drv_id)?;
            self.handle_rebuild_failed_task(&server_drv_id).await?;
        }

        Ok(())
    }
}
