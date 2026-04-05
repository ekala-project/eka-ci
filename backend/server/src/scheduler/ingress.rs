use anyhow::Context;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::db::model::{drv, drv_id};
use crate::graph::GraphServiceHandle;
use crate::scheduler::build::BuildRequest;

/// This acts as the service which filters incoming drv build requests
/// and determines if the drv is "buildable", already successful,
/// already failed, has a dependency failure, otherwise it will mark it as queued.
pub struct IngressService {
    db_service: DbService,
    graph_handle: GraphServiceHandle,
    request_receiver: mpsc::Receiver<IngressTask>,
}

pub struct IngressWorker {
    /// To receive requests for updating or inserting drvs
    request_receiver: mpsc::Receiver<IngressTask>,
    /// To send buildable requests to builder service
    buildable_sender: mpsc::Sender<BuildRequest>,
    db_service: DbService,
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
}

impl IngressService {
    pub fn init(
        db_service: DbService,
        graph_handle: GraphServiceHandle,
    ) -> (Self, mpsc::Sender<IngressTask>) {
        let (request_sender, request_receiver) = mpsc::channel(1000);

        let res = Self {
            db_service,
            graph_handle,
            request_receiver,
        };

        (res, request_sender)
    }

    pub fn run(self, buildable_sender: mpsc::Sender<BuildRequest>) -> JoinHandle<()> {
        let worker = IngressWorker {
            request_receiver: self.request_receiver,
            buildable_sender,
            db_service: self.db_service,
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
        }

        Ok(())
    }

    async fn handle_check_buildable_task(&self, drv_id: drv_id::DrvId) -> anyhow::Result<()> {
        use crate::db::model::build_event::DrvBuildState;
        debug!("checking if {:?} is buildable", &drv_id);

        // Use graph for fast buildability check (no SQL query!)
        if self.graph_handle.is_buildable(&drv_id) {
            debug!("{:?} is now buildable", &drv_id);

            // Get current drv to check its state
            let drv: drv::Drv = self
                .db_service
                .get_drv(&drv_id)
                .await?
                .context("drv is missing")?;

            // Only update state to Buildable if it's not already in FailedRetry
            // FailedRetry state should be preserved so the recorder can detect second failures
            if drv.build_state != DrvBuildState::FailedRetry {
                self.db_service
                    .update_drv_status(&drv_id, &DrvBuildState::Buildable)
                    .await?;
            }

            self.buildable_sender.send(BuildRequest(drv)).await?;
        }

        Ok(())
    }

    /// This attempts to update the status of a drv by inspecting the
    /// status of the dependencies.
    async fn handle_eval_task(&self, drv_id: drv_id::DrvId) -> anyhow::Result<()> {
        // Check if drv is already in a terminal state
        if let Some(drv) = self.db_service.get_drv(&drv_id).await? {
            if drv.build_state.is_terminal() {
                debug!(
                    "{:?} is already in terminal state {:?}, skipping build",
                    &drv_id, drv.build_state
                );
                return Ok(());
            }
        }

        self.handle_check_buildable_task(drv_id).await?;

        Ok(())
    }
}
