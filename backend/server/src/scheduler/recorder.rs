use crate::db::model::{build, build_event, drv};
use crate::db::DbService;
use crate::scheduler::ingress::IngressTask;
use tokio::sync::mpsc;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct RecorderTask {
    derivation: drv::DrvId,
    result: build_event::DrvBuildState,
}

/// This services records the event of a build. Depending on the build result,
/// this service may also enqueue new ingress requests.
///
pub struct RecorderService {
    db_service: DbService,
}

/// Encapsulation of the Recorder thread. May want to gracefully recover from
/// any one particular thread going into a bad state
struct RecorderWorker {
    /// To receive requests for updating or inserting drvs
    ingress_sender: mpsc::Sender<IngressTask>,
    /// To send buildable requests to builder service
    recorder_receiver: mpsc::Receiver<RecorderTask>,
    recorder_sender: mpsc::Sender<RecorderTask>,

    db_service: DbService,
}

impl RecorderService {
    pub fn new(db_service: DbService) -> Self {
        Self { db_service }
    }

    pub fn run(
        &self,
        ingress_sender: mpsc::Sender<IngressTask>,
    ) -> anyhow::Result<mpsc::Sender<RecorderTask>> {
        let worker = RecorderWorker::new(self.db_service.clone(), ingress_sender);

        let recorder_sender = worker.recorder_sender();
        tokio::spawn(async move {
            worker.ingest_requests().await;
        });

        Ok(recorder_sender)
    }
}

impl RecorderWorker {
    fn new(db_service: DbService, ingress_sender: mpsc::Sender<IngressTask>) -> Self {
        let (recorder_sender, recorder_receiver) = mpsc::channel(1000);

        Self {
            db_service,
            ingress_sender,
            recorder_sender,
            recorder_receiver,
        }
    }

    pub fn recorder_sender(&self) -> mpsc::Sender<RecorderTask> {
        self.recorder_sender.clone()
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
        unimplemented!();
    }
}
