use std::process::Output;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use crate::db::model::drv::DrvId;

/// This acts as the service which monitors a "nix build" and reports the
/// status of a build
///
/// TODO: Allow for number of parallel builds to be configured
///       tokio::task::JoinSet would likely be a good option for this
pub struct Builder {
    #[allow(dead_code)]
    build_thread: JoinHandle<()>,
}

impl Builder {
    pub fn new(receiver: mpsc::Receiver<DrvId>, status_sender: mpsc::Sender<DrvId>) -> Self {
        let build_thread = tokio::spawn(async move {
            poll_for_builds(receiver, status_sender).await;
        });

        Self { build_thread }
    }
}

async fn poll_for_builds(
    mut receiver: mpsc::Receiver<DrvId>,
    status_sender: mpsc::Sender<DrvId>,
) {
    loop {
        if let Some(drv_string) = receiver.recv().await {
            attempt_build(drv_string, status_sender.clone()).await;
        }
    }
}

async fn attempt_build(drv_path: DrvId, build_sender: mpsc::Sender<DrvId>) {
    // TODO: Create buildEvent

    match build_drv(&drv_path).await {
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
