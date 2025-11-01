use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::db::model::{build, build_event, drv_id};
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
}

/// Encapsulation of the Recorder thread. May want to gracefully recover from
/// any one particular thread going into a bad state
struct RecorderWorker {
    /// To send buildable requests to builder service
    ingress_sender: mpsc::Sender<IngressTask>,
    recorder_receiver: mpsc::Receiver<RecorderTask>,
    db_service: DbService,
}

impl RecorderService {
    pub fn init(db_service: DbService) -> (Self, mpsc::Sender<RecorderTask>) {
        let (recorder_sender, recorder_receiver) = mpsc::channel(1000);

        let res = Self {
            db_service,
            recorder_receiver,
        };

        (res, recorder_sender)
    }

    pub fn run(self, ingress_sender: mpsc::Sender<IngressTask>) -> JoinHandle<()> {
        let worker = RecorderWorker::new(
            self.db_service.clone(),
            ingress_sender,
            self.recorder_receiver,
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
    ) -> Self {
        Self {
            db_service,
            ingress_sender,
            recorder_receiver,
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
                //let event =
                //    build_event::DrvBuildEvent::for_insert(build_id,
                // DBS::Completed(DBR::Success)); self.db_service.
                // new_drv_build_event(event).await?;

                // Ask ingress service to check if all downstream
                // drvs are now buildable
                let referrers = self.db_service.drv_referrers(&drv).await?;
                debug!("Got {:?} referrers for {}", &referrers, &build_id.derivation.store_path());
                for referrer in referrers {
                    let task = IngressTask::CheckBuildable(referrer);
                    self.ingress_sender.send(task).await?;
                }
            },
            DBS::Completed(DBR::Failure) => {
                // TODO: update status as failure, mark all downstream drvs as
                // dependency failure, and add this drv as cause
                debug!(
                    "Attempting to record failed build of {}",
                    build_id.derivation.store_path()
                );
                self.db_service
                    .update_drv_status(&drv, &task.result)
                    .await?;
                //let event =
                //    build_event::DrvBuildEvent::for_insert(build_id,
                // DBS::Completed(DBR::Failure)); self.db_service.
                // new_drv_build_event(event).await?; TODO: all downstream packages
                // should be set to be a transitiveFailure
            },
            _ => {},
        }

        Ok(())
    }
}
