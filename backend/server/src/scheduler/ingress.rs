use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use anyhow::Context;

use crate::db::DbService;
use crate::db::model::{drv, drv_id};
use crate::scheduler::build::BuildRequest;

/// This acts as the service which filters incoming drv build requests
/// and determines if the drv is "buildable", already successful,
/// already failed, has a dependency failure, otherwise it will mark it as queued.
pub struct IngressService {
    db_service: DbService,
    request_receiver: mpsc::Receiver<IngressTask>,
}

pub struct IngressWorker {
    /// To receive requests for updating or inserting drvs
    request_receiver: mpsc::Receiver<IngressTask>,
    /// To send buildable requests to builder service
    buildable_sender: mpsc::Sender<BuildRequest>,
    db_service: DbService,
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
    pub fn init(db_service: DbService) -> (Self, mpsc::Sender<IngressTask>) {
        let (request_sender, request_receiver) = mpsc::channel(1000);

        let res = Self {
            db_service,
            request_receiver,
        };

        (res, request_sender)
    }

    pub fn run(self, buildable_sender: mpsc::Sender<BuildRequest>) -> JoinHandle<()> {
        let worker = IngressWorker {
            request_receiver: self.request_receiver,
            buildable_sender,
            db_service: self.db_service,
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

        // TODO: make this more generic, we should be checking for other terminal states
        if self.db_service.is_drv_buildable(&drv_id).await? {
            debug!("{:?} is now buildable", &drv_id);
            // let build_id = build::DrvBuildId {
            //     derivation: drv_id.clone(),
            //     // This should not be determined here
            //     build_attempt: std::num::NonZero::new(1).unwrap(),
            // };

            // // TODO: We should scan all of the existing drvs to see if they have
            // // failure state, and instead just immediately propagate it
            // let event = build_event::DrvBuildEvent::for_insert(
            //     build_id,
            //     build_event::DrvBuildState::Buildable,
            // );
            //self.db_service.new_drv_build_event(event).await?;
            self.db_service
                .update_drv_status(&drv_id, &DrvBuildState::Buildable)
                .await?;
            let drv: drv::Drv = self.db_service.get_drv(&drv_id).await?.context("drv is missing")?;
            self.buildable_sender.send(BuildRequest(drv)).await?;
        }

        Ok(())
    }

    /// This attempts to update the status of a drv by inspecting the
    /// status of the dependencies.
    async fn handle_eval_task(&self, drv_id: drv_id::DrvId) -> anyhow::Result<()> {
        // TODO: check if drv is already in a terminal state
        self.handle_check_buildable_task(drv_id).await?;

        Ok(())
    }
}

// Since we have to do a lot of construction, make this its own method
// async fn insert_metadata(
//     drv_id: drv_id::DrvId,
//     db_service: &DbService,
// ) -> anyhow::Result<build::DrvBuildMetadata> {
//     // TODO: Originating source should be passed. For now, just fill
//     // with fake information
//     let repo = git::GitRepo(gix_url::parse(
//         "https://github.com/ekala-project/fake-ci".into(),
//     )?);
//     let commit = git::GitCommit(gix_hash::ObjectId::from_hex(
//         b"1f5cfe6827dc7956af7da54755717202d14667a0",
//     )?);
//     // Eventually, we will want to be able to re-create the .drv on
//     // a potentially remote store, for now, we just assume that the .drv
//     // exists in the local eval store and have this command be dummy data
//     let command = build::DrvBuildCommand::SingleAttr {
//         exec: "/bin/nix".into(),
//         args: Vec::new(),
//         env: HashMap::new(),
//         file: "/path/to/file.nix".into(),
//         attr: "hello".to_owned(),
//     };
//
//     debug!("Inserting DrvMetadata {:?}", &drv_id);
//     let metadata = build::DrvBuildMetadata::for_insert(drv_id, repo, commit, command);
//     db_service.insert_build(metadata).await
// }
