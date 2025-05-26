use super::builder::{BuildRequest, Builder};
use super::ingress::{IngressService, IngressTask};
use super::recorder::RecorderService;
use crate::db::{model::drv, DbService};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

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
    build_request_sender: mpsc::Sender<BuildRequest>,

    builder_thread: JoinHandle<()>,
}

impl SchedulerService {
    pub fn new(db_service: DbService) -> anyhow::Result<Self> {
        let builder_service = Builder::new(db_service.clone());
        let build_request_sender = builder_service.build_request_sender();
        let ingress_service = IngressService::new(build_request_sender.clone(), db_service.clone());
        let ingress_sender = ingress_service.ingress_sender();
        let recorder_service = RecorderService::new(db_service.clone());
        let recorder_task_sender = recorder_service.run(ingress_sender.clone())?;
        let builder_thread = builder_service.run(recorder_task_sender.clone());

        Ok(Self {
            db_service,
            ingress_sender,
            build_request_sender,
            builder_thread,
        })
    }

    /// Producer channel for sending build requests for eventual scheduling
    #[allow(dead_code)]
    pub fn ingress_request_sender(&self) -> mpsc::Sender<IngressTask> {
        self.ingress_sender.clone()
    }
}
