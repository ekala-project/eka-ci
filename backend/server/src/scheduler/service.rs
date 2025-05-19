use super::builder::Builder;
use super::ingress::{IngressService, IngressTask};
use crate::db::DbService;
use tokio::sync::mpsc;

/// SchedulerService spins up three smaller services:
///   RequestIngress:
///         Handles taking in many drv build requests and determines
///         if the drv is "buildable", has a failed dependency, otherwise it's just queued
///   Builder:
///         Receives a stream of "buildable" drvs and builds them.
///         Upon completion of a build, passes build status to RecorderService
///   RecorderService:
///         "Records" build status to the database.
///         Upon recording a successful build, it pushes a check request for
///         each downstream drv to RequestIngress service to see if the drv
///         is now "buildable"
pub struct SchedulerService {
    // Handle to the db service, useful for persisting and querying build state
    db_service: DbService,

    // Channel to send requests for updating drv build progress
    ingress_sender: mpsc::Sender<IngressTask>,

    // When spawning build tasks, we need to pass a clone of this so they
    // can communicate build status asynchronously.
    build_request_sender: mpsc::Sender<String>,

    builder: Builder,
}

impl SchedulerService {
    pub fn new(db_service: DbService) -> Self {
        let (build_request_sender, build_request_receiver) = mpsc::channel(100);
        let (build_result_sender, build_result_receiver) = mpsc::channel(100);

        let builder = Builder::new(build_request_receiver, build_result_sender.clone());
        let ingress_service = IngressService::new( db_service.clone());
        let ingress_sender = ingress_service.ingress_sender();

        Self {
            db_service,
            ingress_sender,
            build_request_sender,
            builder,
        }
    }

    pub fn ingress_request_sender(&self) -> mpsc::Sender<IngressTask> {
        self.ingress_sender.clone()
    }
}
