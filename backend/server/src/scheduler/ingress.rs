use anyhow::Context;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::db::model::build_event::DrvBuildState;
use crate::db::model::drv_id;
use crate::graph::GraphServiceHandle;
use crate::scheduler::build::BuildRequest;

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
    graph_handle: GraphServiceHandle,
}

#[derive(Debug, Clone)]
pub enum IngressTask {
    /// This is a Drv which was determined by an evaluation
    /// The actual status is unknown. Could be new, or could have already completed.
    EvalRequest(drv_id::DrvId),
    /// This is a Drv which we can safely assume had already added and
    /// a dependency was successfully built, and now we should recheck to
    /// to see if the Drv is now buildable
    CheckBuildable(drv_id::DrvId),
    /// Rebuild a failed drv by resetting it to Queued and clearing failure tracking
    RebuildFailed(drv_id::DrvId),
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

    pub fn run(self, buildable_sender: mpsc::Sender<BuildRequest>) -> JoinHandle<()> {
        let worker = IngressWorker {
            request_receiver: self.request_receiver,
            buildable_sender,
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
                if let Err(e) = self.handle_ingress_request(task.clone()).await {
                    warn!("Failed to handle ingress request {:?}: {:?}", &task, e);
                }
            }
        }
    }

    async fn handle_ingress_request(&self, task: IngressTask) -> anyhow::Result<()> {
        use IngressTask::*;

        match task {
            EvalRequest(drv) => self.handle_eval_task(drv).await?,
            CheckBuildable(drv) => self.handle_check_buildable_task(drv).await?,
            RebuildFailed(drv) => self.handle_rebuild_failed_task(drv).await?,
            RebuildAllFailed => self.handle_rebuild_all_failed_task().await?,
        }

        Ok(())
    }

    async fn handle_check_buildable_task(&self, drv_id: drv_id::DrvId) -> anyhow::Result<()> {
        use crate::db::model::build_event::DrvBuildState;
        debug!("checking if {:?} is buildable", &drv_id);

        // Use graph for fast buildability check (no SQL query!)
        if self.graph_handle.is_buildable(&drv_id) {
            debug!("{:?} is now buildable", &drv_id);

            // Get current drv from graph to check its state
            let cached_node = self
                .graph_handle
                .get_node(&drv_id)
                .context("drv is missing from graph")?;

            // Only update state to Buildable if it's not already in FailedRetry
            // FailedRetry state should be preserved so the recorder can detect second failures
            if cached_node.build_state != DrvBuildState::FailedRetry {
                self.graph_handle
                    .update_state(&drv_id, DrvBuildState::Buildable)
                    .await?;
            }

            // Convert CachedNode to Drv for BuildRequest
            let drv = cached_node.to_drv();
            self.buildable_sender.send(BuildRequest(drv)).await?;
        }

        Ok(())
    }

    /// This attempts to update the status of a drv by inspecting the
    /// status of the dependencies.
    async fn handle_eval_task(&self, drv_id: drv_id::DrvId) -> anyhow::Result<()> {
        // Check if drv is already in a terminal state
        if let Some(build_state) = self.graph_handle.get_build_state(&drv_id) {
            if build_state.is_terminal() {
                debug!(
                    "{:?} is already in terminal state {:?}, skipping build",
                    &drv_id, build_state
                );
                return Ok(());
            }
        }

        self.handle_check_buildable_task(drv_id).await?;

        Ok(())
    }

    /// Rebuild a failed drv by resetting it to Queued and clearing failure tracking
    /// This also recursively rebuilds any failed dependencies
    async fn handle_rebuild_failed_task(&self, drv_id: drv_id::DrvId) -> anyhow::Result<()> {
        use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};

        debug!("Attempting to rebuild failed drv: {:?}", &drv_id);

        // Get the drv to check its current state
        let drv = self
            .db_service
            .get_drv(&drv_id)
            .await?
            .context("drv not found")?;

        // Only rebuild if drv is in a failed state
        match &drv.build_state {
            DrvBuildState::Completed(DrvBuildResult::Failure)
            | DrvBuildState::TransitiveFailure
            | DrvBuildState::FailedRetry => {
                debug!(
                    "{:?} is in failed state {:?}, rebuilding",
                    &drv_id, &drv.build_state
                );

                // First, recursively rebuild any failed dependencies
                let failed_deps = self.db_service.get_failed_dependencies(&drv_id).await?;
                for dep in failed_deps {
                    debug!(
                        "{:?} has failed dependency {:?}, rebuilding it first",
                        &drv_id, &dep
                    );
                    Box::pin(self.handle_rebuild_failed_task(dep)).await?;
                }

                // Reset the drv state to Queued in the database
                self.db_service
                    .update_drv_status(&drv_id, &DrvBuildState::Queued)
                    .await?;

                // Clear transitive failure records in the database
                // This unblocks all transitively failed dependents
                self.db_service.clear_transitive_failures(&drv_id).await?;

                // Re-queue the drv to check if it's now buildable
                self.handle_check_buildable_task(drv_id.clone()).await?;

                debug!("{:?} has been reset and re-queued", &drv_id);
            },
            _ => {
                warn!(
                    "{:?} is not in a failed state (current state: {:?}), cannot rebuild",
                    &drv_id, &drv.build_state
                );
            },
        }

        Ok(())
    }

    /// Rebuild all failed drvs in the system
    async fn handle_rebuild_all_failed_task(&self) -> anyhow::Result<()> {
        debug!("Rebuilding all failed drvs");

        // Get all failed drvs from the database
        let failed_drvs = self.db_service.get_all_failed_drvs().await?;

        debug!("Found {} failed drvs to rebuild", failed_drvs.len());

        // Rebuild each failed drv (this will handle dependencies recursively)
        for drv_id in failed_drvs {
            self.handle_rebuild_failed_task(drv_id).await?;
        }

        Ok(())
    }
}
