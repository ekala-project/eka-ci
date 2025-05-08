use crate::db::model::{build, drv, git};
use crate::db::{insert, DbService};
use std::collections::HashMap;
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
        request_receiver: mpsc::Receiver<drv::DrvId>,
        buildable_sender: mpsc::Sender<drv::DrvId>,
        db_service: DbService,
    ) -> Self {
        let db_clone = db_service.clone();
        let ingress_thread = tokio::spawn(async move {
            ingest_requests(request_receiver, buildable_sender, db_clone).await;
        });

        Self {
            ingress_thread,
            db_service,
        }
    }
}

async fn ingest_requests(
    mut request_receiver: mpsc::Receiver<drv::DrvId>,
    buildable_sender: mpsc::Sender<drv::DrvId>,
    db_service: DbService,
) {
    loop {
        if let Some(drv_id) = request_receiver.recv().await {
            // TODO: Determine if drv_id corresponds to a drv which was already attempted
            //         if it has a previous terminal state, disregard
            //         if one of the dependencies has a failure, set state as dependency failed
            //         otherwise set as queued, move on
            if let Err(e) = insert_metadata(drv_id, &db_service).await {
                warn!("Encountered error while inserting metadata: {:?}", e);
            }
        }
    }
}

/// Since we have to do a lot of construction, make this its own method
async fn insert_metadata(drv_id: drv::DrvId, db_service: &DbService) -> anyhow::Result<()> {
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
    let metadata = insert::new_drv_build_metadata(metadata, &db_service.pool).await?;

    Ok(())
}
