use crate::db::model::drv::DrvId;
use crate::scheduler::recorder::RecorderTask;
use std::process::Output;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

pub struct BuildRequest(pub DrvId);

/// This acts as the service which monitors a "nix build" and reports the
/// status of a build
///
/// TODO: Allow for number of parallel builds to be configured
///       tokio::task::JoinSet would likely be a good option for this
pub struct Builder {
    build_request_sender: mpsc::Sender<BuildRequest>,
    build_request_receiver: mpsc::Receiver<BuildRequest>,
}

impl Builder {
    /// Immediately starts builder service
    pub fn new() -> Self {
        let (build_request_sender, build_request_receiver) = mpsc::channel(100);

        Self {
            build_request_sender,
            build_request_receiver,
        }
    }

    pub fn build_request_sender(&self) -> mpsc::Sender<BuildRequest> {
        self.build_request_sender.clone()
    }

    pub fn run(self, recorder_sender: mpsc::Sender<RecorderTask>) -> JoinHandle<()> {
        let recorder_clone = recorder_sender.clone();
        tokio::spawn(async move {
            poll_for_builds(self.build_request_receiver, recorder_clone).await;
        })
    }
}

async fn poll_for_builds(
    mut receiver: mpsc::Receiver<BuildRequest>,
    status_sender: mpsc::Sender<RecorderTask>,
) {
    loop {
        if let Some(drv_string) = receiver.recv().await {
            attempt_build(drv_string, &status_sender).await;
        }
    }
}

async fn attempt_build(build_request: BuildRequest, recorder_sender: &mpsc::Sender<RecorderTask>) {
    // TODO: Create buildEvent
    let drv_path = &build_request.0;

    match build_drv(drv_path).await {
        Ok(output) => {
            if output.status.success() {
                // TODO: Update buildEvent with completed success
                debug!("Successfully built {:?}", drv_path);
            } else {
                // TODO: Update buildEvent with completed failure
                debug!("Successfully built {:?}", drv_path);
            }
        }

        // Err doesn't denote process failure, rather process construction
        Err(e) => {
            // TODO: Update buildEvent with failure
            warn!("Failed to build {:?}, encountered error: {:?}", drv_path, e);
        }
    }
}

async fn build_drv(drv_path: &str) -> anyhow::Result<Output> {
    let build_output = Command::new("nix-store")
        .args(["--realise", drv_path])
        .output()
        .await?;

    Ok(build_output)
}
