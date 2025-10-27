use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::build::{BuildQueue, BuildRequest, Builder};
use super::ingress::{IngressService, IngressTask};
use super::recorder::{RecorderService, RecorderTask};
use crate::config::RemoteBuilder;
use crate::db::DbService;

/// SchedulerService spins up three smaller services:
///   RequestIngress:
///         Handles taking in many drv build requests and determines
///         if the drv is "buildable", has a failed dependency, otherwise it's just queued
///   BuildQueue:
///         Receives a stream of "buildable" drvs and builds them.
///         Upon completion of a build, passes build status to RecorderService
///   RecorderService:
///         "Records" build status to the database.
///         Upon recording a successful build, it pushes a check request for
///         each downstream drv to RequestIngress service to see if the drv
///         is now "buildable"
#[allow(dead_code)]
pub struct SchedulerService {
    // Handle to the db service, useful for persisting and querying build state
    db_service: DbService,

    // Channels to individual services
    // We may in the future need to recover an individual service, so retaining
    // a handle to the other service channels will be prequisite
    ingress_sender: mpsc::Sender<IngressTask>,
    builder_sender: mpsc::Sender<BuildRequest>,
    recorder_sender: mpsc::Sender<RecorderTask>,

    ingress_thread: JoinHandle<()>,
    builder_thread: JoinHandle<()>,
    recorder_thread: JoinHandle<()>,
}

impl SchedulerService {
    pub fn new(
        db_service: DbService,
        remote_builders: Vec<RemoteBuilder>,
        local_builders: Vec<Builder>,
    ) -> anyhow::Result<Self> {
        let (ingress_service, ingress_sender) = IngressService::init(db_service.clone());
        let (recorder_service, recorder_sender) = RecorderService::init(db_service.clone());
        let (builder_service, builder_sender) =
            BuildQueue::init(db_service.clone(), remote_builders, local_builders, recorder_sender.clone());
        let ingress_thread = ingress_service.run(builder_sender.clone());
        let recorder_thread = recorder_service.run(ingress_sender.clone());
        let builder_thread = builder_service.run(recorder_sender.clone());

        Ok(Self {
            db_service,
            ingress_sender,
            builder_sender,
            recorder_sender,
            ingress_thread,
            builder_thread,
            recorder_thread,
        })
    }

    /// Producer channel for sending build requests for eventual scheduling
    pub fn ingress_request_sender(&self) -> mpsc::Sender<IngressTask> {
        self.ingress_sender.clone()
    }
}
