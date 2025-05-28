use crate::db::model::{build, build_event, drv};
use crate::db::DbService;
use crate::scheduler::ingress::IngressTask;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct RecorderTask {
    pub derivation: drv::DrvId,
    pub result: build_event::DrvBuildState,
}

/// This services records the event of a build. Depending on the build result,
/// this service may also enqueue new ingress requests.
///
pub struct RecorderService {
    db_service: DbService,
    recorder_receiver: mpsc::Receiver<RecorderTask>,
    recorder_sender: mpsc::Sender<RecorderTask>,
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
    pub fn new(db_service: DbService) -> Self {
        let (recorder_sender, recorder_receiver) = mpsc::channel(1000);
        Self {
            db_service,
            recorder_receiver,
            recorder_sender,
        }
    }

    pub fn recorder_sender(&self) -> mpsc::Sender<RecorderTask> {
        self.recorder_sender.clone()
    }

    pub fn run(
        self,
        ingress_sender: mpsc::Sender<IngressTask>,
    ) -> JoinHandle<()> {
        let worker = RecorderWorker::new(self.db_service.clone(), ingress_sender, self.recorder_receiver);

        tokio::spawn(async move {
            worker.ingest_requests().await;
        })
    }
}

impl RecorderWorker {
    fn new(db_service: DbService, ingress_sender: mpsc::Sender<IngressTask>, recorder_receiver: mpsc::Receiver<RecorderTask>) -> Self {

        Self {
            db_service,
            ingress_sender,
            recorder_receiver,
        }
    }

    async fn ingest_requests(mut self) {
        loop {
            if let Some(task) = self.recorder_receiver.recv().await {
                if let Err(e) = self.handle_recorder_request(task.clone()).await {
                    warn!("Failed to handle ingress request {:?}: {:?}", &task, e);
                }
            }
        }
    }

    async fn handle_recorder_request(&self, task: RecorderTask) -> anyhow::Result<()> {
        use build_event::*;
        use DrvBuildState as DBS;
        use DrvBuildResult as DBR;

        match &task.result {
            DBS::Completed(DBR::Success) => {
                // TODO: update status as successful, ask ingress to check drv again
            },
            DBS::Completed(DBR::Failure) => {
                // TODO: update status as failure, mark all downstream drvs as
                // dependency failure, and add this drv as cause
            },
            _ => { },
        }

        Ok(())
    }
}
