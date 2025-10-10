use std::process::Output;

use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::db::model::build::DrvBuildId;
use crate::db::model::build_event;
use crate::db::model::drv_id::DrvId;
use crate::scheduler::recorder::RecorderTask;

pub struct BuildRequest(pub DrvId);

/// This acts as the service which monitors a "nix build" and reports the
/// status of a build
///
/// TODO: Allow for number of parallel builds to be configured
///       tokio::task::JoinSet would likely be a good option for this
pub struct Builder {
    db_service: DbService,
    build_request_receiver: mpsc::Receiver<BuildRequest>,
}

impl Builder {
    /// Immediately starts builder service
    pub fn init(db_service: DbService) -> (Self, mpsc::Sender<BuildRequest>) {
        let (build_request_sender, build_request_receiver) = mpsc::channel(100);

        let res = Self {
            db_service,
            build_request_receiver,
        };

        (res, build_request_sender)
    }

    pub fn run(self, recorder_sender: mpsc::Sender<RecorderTask>) -> JoinHandle<()> {
        tokio::spawn(async move {
            poll_for_builds(
                self.build_request_receiver,
                self.db_service,
                recorder_sender,
            )
            .await;
        })
    }
}

async fn poll_for_builds(
    mut receiver: mpsc::Receiver<BuildRequest>,
    db_service: DbService,
    status_sender: mpsc::Sender<RecorderTask>,
) {
    loop {
        if let Some(drv_string) = receiver.recv().await {
            debug!("Attempting to build {:?}", &drv_string.0);
            let res = attempt_build(&drv_string, &db_service, &status_sender).await;
            match res {
                Ok(_) => {
                    debug!("The build attempt for {:?} has completed", &drv_string.0);
                },
                Err(e) => {
                    debug!(
                        "The build attempt for {:?} has failed with: {}",
                        &drv_string.0, e
                    );
                },
            }
        }
    }
}

async fn attempt_build(
    build_request: &BuildRequest,
    db_service: &DbService,
    recorder_sender: &mpsc::Sender<RecorderTask>,
) -> anyhow::Result<()> {
    use build_event::{DrvBuildEvent, DrvBuildState};
    let drv_path = build_request.0.clone();
    let build_id = DrvBuildId {
        derivation: drv_path.clone(),
        // TODO: build_attempt seems like something we should query
        build_attempt: std::num::NonZeroU32::new(1).unwrap(),
    };

    let buildable_event = DrvBuildEvent::for_insert(build_id, DrvBuildState::Building);
    db_service.new_drv_build_event(buildable_event).await?;

    let build_state = perform_build(&drv_path).await;

    // To avoid the state of the build not pushing the result to other potential
    // drvs, we let the recorder deal with updating the build_event task
    // and determining if other drv's now can be queued
    let recorder_task = RecorderTask {
        derivation: drv_path,
        result: build_state.clone(),
    };

    recorder_sender.send(recorder_task).await?;

    Ok(())
}

async fn perform_build(drv: &DrvId) -> build_event::DrvBuildState {
    use build_event::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};

    let drv_path = drv.store_path();
    match build_drv(&drv_path).await {
        Ok(output) => {
            if output.status.success() {
                debug!("Successfully built {:?}", drv_path);
                DrvBuildState::Completed(DrvBuildResult::Success)
            } else {
                debug!("Build failed for {:?}", drv_path);
                DrvBuildState::Completed(DrvBuildResult::Failure)
            }
        },

        // Err doesn't denote process failure, rather process construction
        Err(e) => {
            warn!("Failed to build {:?}, encountered error: {:?}", drv_path, e);
            DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath)
        },
    }
}

async fn build_drv(drv_path: &str) -> anyhow::Result<Output> {
    let build_output = Command::new("nix-store")
        .args(["--realise", drv_path])
        .output()
        .await?;

    Ok(build_output)
}
