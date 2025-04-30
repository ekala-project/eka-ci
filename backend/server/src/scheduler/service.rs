use crate::db::DbService;
use std::process::Output;
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub struct SchedulerService {
    // Handle to the db service, useful for persisting and querying build state
    db_service: DbService,

    // Channel to be notified of new drvs
    // TODO: Use DrvId or Drv (with DrvId)
    new_drv_receiver: mpsc::Receiver<String>,

    // Channel for this service to listen on. Receive build status from builders.
    build_receiver: mpsc::Receiver<String>,

    // When spawning build tasks, we need to pass a clone of this so they
    // can communicate build status asynchronously.
    build_sender: mpsc::Sender<String>,
}

impl SchedulerService {
    pub fn new(db_service: DbService, new_drv_receiver: mpsc::Receiver<String>) -> Self {
        let (build_sender, build_receiver) = mpsc::channel(100);

        Self {
            db_service,
            new_drv_receiver,
            build_receiver,
            build_sender,
        }
    }

    async fn attempt_build(drv_path: String, build_sender: mpsc::Sender<String>) {
        // TODO: Create buildEvent

        match build_drv(&drv_path).await {
            Ok(output) => {
                if output.status.success() {
                    // TODO: Update buildEvent with completed success
                    debug!("Successfully built {}", drv_path);
                } else {
                    // TODO: Update buildEvent with completed failure
                    debug!("Successfully built {}", drv_path);
                }
            }

            // Err doesn't denote process failure, rather process construction
            Err(e) => {
                // TODO: Update buildEvent with failure
                warn!("Failed to build {}, encountered error: {:?}", drv_path, e);
            }
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
