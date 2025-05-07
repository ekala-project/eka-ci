use crate::db::DbService;
use tokio::sync::mpsc;
use super::builder::Builder;

/// SchedulerService spins up three smaller services:
///   RequestIngress:
///         Handles taking in many drv build requests and determines
///         if the drv is "buildable", has a failed dependency, otherwise it's just queued
///   Builder:
///         Receives a stream of "buildable" drvs and builds them.
///         Upon completion of a build, passes build status to RecorderService
///   RecorderService:
///         "Records" build status to the database. Upon recording a gg
pub struct SchedulerService {
    // Handle to the db service, useful for persisting and querying build state
    db_service: DbService,

    // Channel to be notified of new drvs
    // TODO: Use DrvId or Drv (with DrvId)
    new_drv_receiver: mpsc::Receiver<String>,

    // When spawning build tasks, we need to pass a clone of this so they
    // can communicate build status asynchronously.
    build_request_sender: mpsc::Sender<String>,

    builder: Builder,
}

impl SchedulerService {
    pub fn new(db_service: DbService, new_drv_receiver: mpsc::Receiver<String>) -> Self {
        let (build_request_sender, build_request_receiver) = mpsc::channel(100);
        let (build_result_sender, build_result_receiver) = mpsc::channel(100);

        let builder = Builder::new(build_request_receiver, build_result_sender.clone());

        Self {
            db_service,
            new_drv_receiver,
            build_request_sender,
            builder,
        }
    }

}


