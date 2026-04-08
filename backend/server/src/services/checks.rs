use std::path::Path;

use anyhow::{Context, Result};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::checks::executor::execute_check;
use crate::checks::types::{CheckResultMessage, CheckTask};
use crate::db::DbService;
use crate::github::GitHubTask;

pub struct ChecksExecutor {
    check_receiver: mpsc::Receiver<CheckTask>,
    db_service: DbService,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
}

impl ChecksExecutor {
    pub fn new(
        check_receiver: mpsc::Receiver<CheckTask>,
        db_service: DbService,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
    ) -> Self {
        Self {
            check_receiver,
            db_service,
            github_sender,
        }
    }

    pub async fn run(mut self, cancellation_token: CancellationToken) {
        while let Some(request) = cancellation_token
            .run_until_cancelled(self.check_receiver.recv())
            .await
        {
            let task = match request {
                Some(task) => task,
                None => {
                    warn!("Check receiver channel closed, shutting down");
                    break;
                },
            };

            if let Err(e) = self.handle_check_task(task).await {
                error!(error = %e, "Failed to handle check task");
            }
        }

        info!("ChecksExecutor service shutdown gracefully");
    }

    async fn handle_check_task(&mut self, task: CheckTask) -> Result<()> {
        info!(
            "Executing check '{}' for {}/{}@{}",
            task.check_name, task.owner, task.repo_name, task.sha
        );

        // Clone the repository to a temporary directory
        let temp_dir = tempfile::tempdir().context("failed to create temp directory")?;
        let checkout_path = temp_dir.path();

        // Clone and checkout the specific SHA
        let clone_url = format!("https://github.com/{}/{}.git", task.owner, task.repo_name);
        self.clone_and_checkout(&clone_url, &task.sha, checkout_path)
            .await
            .context("failed to clone repository")?;

        // Execute the check in a sandboxed environment
        let result = execute_check(&task.config, checkout_path, &task.check_name)
            .await
            .context("failed to execute check")?;

        info!(
            "Check '{}' completed: success={}, exit_code={}, duration={}ms",
            task.check_name, result.success, result.exit_code, result.duration_ms
        );

        // Store the check result in the database
        let checkset_id = self
            .db_service
            .insert_github_checkset(&task.sha, &task.check_name, &task.owner, &task.repo_name)
            .await
            .context("failed to insert checkset")?;

        let _check_result_id = self
            .db_service
            .insert_check_result(
                checkset_id,
                result.success,
                result.exit_code,
                &result.stdout,
                &result.stderr,
                result.duration_ms as i64,
            )
            .await
            .context("failed to insert check result")?;

        // Report the result back to GitHub if we have a GitHub sender
        if let Some(github_sender) = &self.github_sender {
            // Get the check_run_id from the database
            // We need to query it because it was created when the check was initiated
            if let Some(check_run_info) = self
                .db_service
                .get_check_run_info_by_checkset(&task.owner, &task.repo_name, checkset_id)
                .await
                .context("failed to get check run info")?
            {
                let result_message = CheckResultMessage {
                    check_name: task.check_name.clone(),
                    owner: task.owner.clone(),
                    repo_name: task.repo_name.clone(),
                    sha: task.sha.clone(),
                    success: result.success,
                    exit_code: result.exit_code,
                    stdout: result.stdout.clone(),
                    stderr: result.stderr.clone(),
                    duration_ms: result.duration_ms as i64,
                    check_run_id: check_run_info.check_run_id,
                };

                let github_task = if result.success {
                    GitHubTask::CheckComplete(result_message)
                } else {
                    GitHubTask::CheckFailed(result_message)
                };

                github_sender
                    .send(github_task)
                    .await
                    .context("failed to send GitHub task")?;
            } else {
                warn!(
                    "No check_run_id found for check '{}' in {}/{}",
                    task.check_name, task.owner, task.repo_name
                );
            }
        }

        Ok(())
    }

    async fn clone_and_checkout(&self, clone_url: &str, sha: &str, path: &Path) -> Result<()> {
        debug!("Cloning {} to {:?}", clone_url, path);

        // Clone the repository
        let clone_output = Command::new("git")
            .args(["clone", clone_url, path.to_str().unwrap()])
            .output()
            .await
            .context("failed to execute git clone")?;

        if !clone_output.status.success() {
            anyhow::bail!(
                "git clone failed: {}",
                String::from_utf8_lossy(&clone_output.stderr)
            );
        }

        // Checkout the specific SHA
        debug!("Checking out SHA {} in {:?}", sha, path);
        let checkout_output = Command::new("git")
            .current_dir(path)
            .args(["checkout", sha])
            .output()
            .await
            .context("failed to execute git checkout")?;

        if !checkout_output.status.success() {
            anyhow::bail!(
                "git checkout failed: {}",
                String::from_utf8_lossy(&checkout_output.stderr)
            );
        }

        Ok(())
    }
}
