use crate::db::model::{build, build_event, drv, git};
use crate::db::DbService;
use crate::scheduler::builder::BuildRequest;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// This acts as the service which filters incoming drv build requests
/// and determines if the drv is "buildable", already successful,
/// already failed, has a dependency failure, otherwise it will mark it as queued.
///
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
    EvalRequest(drv::DrvId),
    /// This is a Drv which we can safely assume had already added and
    /// a dependency was successfully built, and now we should recheck to
    /// to see if the Drv is now buildable
    CheckBuildable(drv::DrvId),
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

    async fn handle_check_buildable_task(&self, drv_id: drv::DrvId) -> anyhow::Result<()> {
        // TODO: make this more generic, we should be checking for other terminal states
        if self.db_service.is_drv_buildable(&drv_id).await? {
            let build_id = build::DrvBuildId {
                derivation: drv_id.clone(),
                // This should not be determined here
                build_attempt: std::num::NonZero::new(1).unwrap(),
            };

            // TODO: We should scan all of the existing drvs to see if they have
            // failure state, and instead just immediately propagate it
            let event = build_event::DrvBuildEvent::for_insert(
                build_id,
                build_event::DrvBuildState::Buildable,
            );
            self.db_service.new_drv_build_event(event).await?;
            self.buildable_sender.send(BuildRequest(drv_id)).await?;
        }

        Ok(())
    }

    async fn handle_eval_task(&self, drv_id: drv::DrvId) -> anyhow::Result<()> {
        // For evaluation tasks, we only really need to do an action if the drv
        // hasn't been encountered before. Check if there's a build_event for this drv
        let maybe_build_event = self.db_service.get_latest_build_event(&drv_id).await?;
        if let None = maybe_build_event {
            // No event is associated, create new
            match insert_metadata(drv_id, &self.db_service).await {
                Err(e) => warn!("Encountered error while inserting metadata: {:?}", e),
                Ok(metadata) => {
                    // Since we can't guarantee that all input drvs have been
                    // added to the db, we add check request to the end of the request queue.
                    // This is to avoid a downstream drv that hasn't yet had their
                    // dependencies yet added to appear as though it is a drv that doesn't have
                    // any inputDrvs
                    let build_id = build::DrvBuildId {
                        derivation: metadata.build.derivation,
                        build_attempt: std::num::NonZero::new(0).unwrap(),
                    };

                    // TODO: We should scan all of the existing drvs to see if they have
                    // failure state, and instead just immediately propagate it
                    let event = build_event::DrvBuildEvent::for_insert(
                        build_id,
                        build_event::DrvBuildState::Queued,
                    );
                    self.db_service.new_drv_build_event(event).await?;
                }
            }
        }

        Ok(())
    }
}

/// Since we have to do a lot of construction, make this its own method
async fn insert_metadata(
    drv_id: drv::DrvId,
    db_service: &DbService,
) -> anyhow::Result<build::DrvBuildMetadata> {
    // TODO: Originating source should be passed. For now, just fill
    // with fake information
    let repo = git::GitRepo(gix_url::parse(
        "https://github.com/ekala-project/fake-ci".into(),
    )?);
    let commit = git::GitCommit(gix_hash::ObjectId::from_hex(
        b"1f5cfe6827dc7956af7da54755717202d14667a0",
    )?);
    // Eventually, we will want to be able to re-create the .drv on
    // a potentially remote store, for now, we just assume that the .drv
    // exists in the local eval store and have this command be dummy data
    let command = build::DrvBuildCommand::SingleAttr {
        exec: "/bin/nix".into(),
        args: Vec::new(),
        env: HashMap::new(),
        file: "/path/to/file.nix".into(),
        attr: "hello".to_owned(),
    };

    debug!("Inserting DrvMetadata {:?}", &drv_id);
    let metadata = build::DrvBuildMetadata::for_insert(drv_id, repo, commit, command);
    db_service.insert_build(metadata).await
}
