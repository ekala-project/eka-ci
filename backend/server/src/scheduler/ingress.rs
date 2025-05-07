use crate::db::model::drv;
use crate::db::DbService;
use std::process::Output;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// This acts as the service which filters incoming drv build requests
/// and determines if the drv is "buildable", already successful,
/// already failed, has a dependency failure, otherwise it will mark it as queued.
///
pub struct Ingress {
    db_service: DbService,

    #[allow(dead_code)]
    ingress_thread: JoinHandle<()>,
}

impl Ingress {
    pub fn new(
        receiver: mpsc::Receiver<String>,
        status_sender: mpsc::Sender<String>,
        db_service: DbService,
    ) -> Self {
        let db_clone = db_service.clone();
        let ingress_thread = tokio::spawn(async move {
            ingest_requests(receiver, status_sender, db_service);
        });

        Self {
            ingress_thread,
            db_service,
        }
    }
}

async fn ingest_requests(
    mut receiver: mpsc::Receiver<drv::DrvId>,
    buildable_sender: mpsc::Sender<drv::DrvId>,
    db_service: DbService,
) {
    loop {
        if let Some(drv_id) = receiver.recv().await {
            // TODO: Determine if drv_id corresponds to a drv which was already attempted
            //         if it has a previous terminal state, disregard
            //         if one of the dependencies has a failure, set state as dependency failed
            //         otherwise set as queued, move on
        }
    }
}
