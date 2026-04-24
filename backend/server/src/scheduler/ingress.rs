use std::sync::Arc;

use anyhow::Context;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};
use crate::db::model::drv_id;
use crate::graph::GraphServiceHandle;
use crate::scheduler::build::BuildRequest;
use crate::scheduler::recorder::RecorderTask;

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
}

/// Variants carry `Arc<DrvId>` so fan-out senders (recorder, webhooks,
/// nix eval) can `Arc::clone` instead of cloning the inner `String`
/// when the same drv crosses multiple channel hops.
#[derive(Debug, Clone)]
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
        let (request_sender, request_receiver) = mpsc::channel(1000);

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
    ) -> JoinHandle<()> {
        let worker = IngressWorker {
            request_receiver: self.request_receiver,
            buildable_sender,
            recorder_sender,
            graph_handle: self.graph_handle,
        };
        tokio::spawn(async move {
            worker.ingest_requests().await;
        })
    }
}

impl IngressWorker {
    async fn ingest_requests(mut self) {
        loop {
            if let Some(task) = self.request_receiver.recv().await {
                if let Err(e) = self.handle_ingress_request(&task).await {
                    warn!("Failed to handle ingress request {:?}: {:?}", &task, e);
                }
            }
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
        debug!("checking if {:?} is buildable", drv_id);

        if self.graph_handle.is_buildable(drv_id) {
            debug!("{:?} is now buildable", drv_id);

            let cached_node = self
                .graph_handle
                .get_node(drv_id)
                .context("drv is missing from graph")?;

            // FailedRetry must be preserved so the recorder can detect second failures.
            if cached_node.build_state != DrvBuildState::FailedRetry {
                self.graph_handle
                    .update_state(drv_id, DrvBuildState::Buildable)
                    .await?;
            }

            let drv = cached_node.to_drv();
            self.buildable_sender.send(BuildRequest(drv)).await?;
        }

        Ok(())
    }

    /// This attempts to update the status of a drv by inspecting the
    /// status of the dependencies.
    async fn handle_eval_task(&self, drv_id: &drv_id::DrvId) -> anyhow::Result<()> {
        if let Some(build_state) = self.graph_handle.get_build_state(drv_id) {
            if build_state.is_terminal() {
                debug!(
                    "{:?} is already in terminal state {:?}, skipping build",
                    drv_id, build_state
                );
                return Ok(());
            }
        }

        // Try to short-circuit via substitution before queuing a real build.
        // A successful cache hit marks this drv (and any cached requisites) as
        // Completed(Success) via the recorder, which will fan out CheckBuildable
        // to dependents — no need to fall through.
        match self.handle_check_substitution_task(drv_id).await {
            Ok(true) => {
                debug!("{} short-circuited via substitution", drv_id.store_path());
                return Ok(());
            },
            Ok(false) => { /* not cached; fall through */ },
            Err(e) => {
                // Best-effort optimization — log and proceed to build.
                warn!(
                    "substitution check errored for {} (falling back to build): {:?}",
                    drv_id.store_path(),
                    e
                );
            },
        }

        self.handle_check_buildable_task(drv_id).await?;

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

        // Root must be cached (i.e. NOT in will_build) to short-circuit.
        if !report.is_cached(drv_id) {
            return Ok(false);
        }

        debug!(
            "substitution hit for {} ({} requisites would be built, {} fetched)",
            drv_id.store_path(),
            report.will_build.len(),
            report.will_fetch.len()
        );

        // Collect the root + all transitively-cached requisites. We walk
        // `nix-store --query --requisites` (which returns the root plus all
        // transitive deps) and keep only the ones NOT marked as
        // "will be built", since those are the cache hits.
        let mut cache_hits: Vec<drv_id::DrvId> = Vec::new();
        match crate::nix::drv_requisites(&drv_id.store_path()).await {
            Ok(requisites) => {
                for req in requisites {
                    if !report.will_build.contains(&req.store_path()) {
                        cache_hits.push(req);
                    }
                }
            },
            Err(e) => {
                // If we can't enumerate requisites, fall back to marking just
                // the root drv. The dependents-cascade in the recorder will
                // still correctly re-trigger child evaluation via existing
                // CheckBuildable flow.
                warn!(
                    "failed to enumerate requisites for {} (marking root only): {:?}",
                    drv_id.store_path(),
                    e
                );
                cache_hits.push(drv_id.clone());
            },
        }

        // Always include the root, in case it wasn't returned by --requisites
        // (defensive — the requisites query normally includes the root, but
        // we protect against any future behavior change).
        if !cache_hits.iter().any(|d| d == drv_id) {
            cache_hits.push(drv_id.clone());
        }

        // For each cache hit, only emit a recorder task if the current state
        // is not already terminal AND not currently building. This avoids
        // racing with an in-flight build or clobbering a previously recorded
        // success/failure.
        for hit in cache_hits {
            let current = self.graph_handle.get_build_state(&hit);
            let safe_to_mark = match &current {
                None => true, // not in graph yet; recorder will reject if no DB row
                Some(DrvBuildState::Building) => false,
                Some(state) if state.is_terminal() => false,
                Some(_) => true,
            };
            if !safe_to_mark {
                debug!(
                    "skipping cache-hit mark for {} (state: {:?})",
                    hit.store_path(),
                    current
                );
                continue;
            }

            let task = RecorderTask {
                derivation: Arc::new(hit),
                result: DrvBuildState::Completed(DrvBuildResult::Success),
            };
            // If recorder is gone we can't make progress; surface the error.
            self.recorder_sender
                .send(task)
                .await
                .context("recorder channel closed while recording cache hits")?;
        }

        Ok(true)
    }

    /// Rebuild a failed drv by resetting it to Queued and clearing failure tracking.
    /// Also recursively rebuilds any failed dependencies.
    async fn handle_rebuild_failed_task(&self, drv_id: &drv_id::DrvId) -> anyhow::Result<()> {
        use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};

        debug!("Attempting to rebuild failed drv: {:?}", drv_id);

        let node = self
            .graph_handle
            .get_node(drv_id)
            .context("drv not found")?;

        match &node.build_state {
            DrvBuildState::Completed(DrvBuildResult::Failure)
            | DrvBuildState::TransitiveFailure
            | DrvBuildState::FailedRetry => {
                debug!(
                    "{:?} is in failed state {:?}, rebuilding",
                    drv_id, &node.build_state
                );

                let failed_deps = self.graph_handle.get_failed_dependencies(drv_id).await?;
                for dep in &failed_deps {
                    debug!(
                        "{:?} has failed dependency {:?}, rebuilding it first",
                        drv_id, dep
                    );
                    Box::pin(self.handle_rebuild_failed_task(dep)).await?;
                }

                self.graph_handle
                    .update_state(drv_id, DrvBuildState::Queued)
                    .await?;

                // Transitive-failure cleanup happens in the graph service's
                // ClearFailure command when the drv succeeds.

                self.handle_check_buildable_task(drv_id).await?;

                debug!("{:?} has been reset and re-queued", drv_id);
            },
            _ => {
                warn!(
                    "{:?} is not in a failed state (current state: {:?}), cannot rebuild",
                    drv_id, &node.build_state
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

        for drv_id in &failed_drvs {
            self.handle_rebuild_failed_task(drv_id).await?;
        }

        Ok(())
    }
}
