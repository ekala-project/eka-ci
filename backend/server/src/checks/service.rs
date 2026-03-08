use std::path::PathBuf;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use super::executor::execute_check;
use crate::ci::config::Check;
use crate::db::DbService;
use crate::github::CICheckInfo;

#[derive(Debug, Clone)]
pub struct CheckTask {
    pub check_name: String,
    pub check: Check,
    pub repo_path: PathBuf,
    pub ci_info: CICheckInfo,
}

/// ChecksExecutor service executes checks in sandboxed environments with dev shells
pub struct ChecksExecutor {
    db_service: DbService,
    checks_receiver: mpsc::Receiver<CheckTask>,
}

/// Worker thread that processes check execution requests
struct ChecksWorker {
    checks_receiver: mpsc::Receiver<CheckTask>,
    // Kept for future use when implementing check result storage (see TODOs in handle_check_task)
    #[allow(dead_code)]
    db_service: DbService,
}

impl ChecksExecutor {
    pub fn init(db_service: DbService) -> (Self, mpsc::Sender<CheckTask>) {
        let (checks_sender, checks_receiver) = mpsc::channel(100);

        let service = Self {
            db_service,
            checks_receiver,
        };

        (service, checks_sender)
    }

    pub fn run(self) -> JoinHandle<()> {
        let worker = ChecksWorker::new(self.db_service.clone(), self.checks_receiver);

        tokio::spawn(async move {
            worker.process_checks().await;
        })
    }
}

impl ChecksWorker {
    fn new(db_service: DbService, checks_receiver: mpsc::Receiver<CheckTask>) -> Self {
        Self {
            db_service,
            checks_receiver,
        }
    }

    async fn process_checks(mut self) {
        loop {
            if let Some(task) = self.checks_receiver.recv().await {
                debug!(
                    "Received check task: {} for {}/{}@{}",
                    task.check_name, task.ci_info.owner, task.ci_info.repo_name, task.ci_info.commit
                );
                if let Err(e) = self.handle_check_task(task.clone()).await {
                    warn!("Failed to handle check task {:?}: {:?}", &task.check_name, e);
                }
            }
        }
    }

    async fn handle_check_task(&self, task: CheckTask) -> anyhow::Result<()> {
        let result = execute_check(
            &task.check,
            &task.repo_path,
            &task.check_name,
            &task.ci_info,
        )
        .await?;

        debug!(
            "Check {} completed with exit code {} (success: {})",
            task.check_name, result.exit_code, result.success
        );

        // TODO: Store check results in database
        // TODO: Report check results to GitHub via check runs

        Ok(())
    }
}
