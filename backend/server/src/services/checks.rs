use std::path::Path;

use anyhow::{Context, Result};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::checks::executor::execute_check;
use crate::checks::types::{CheckResultMessage, CheckTask};
use crate::db::DbService;
use crate::github::GitHubTask;
use crate::services::{AsyncService, TaskJournal};

pub struct ChecksExecutor {
    check_sender: mpsc::Sender<CheckTask>,
    check_receiver: Option<mpsc::Receiver<CheckTask>>,
    db_service: DbService,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    journal: TaskJournal<CheckTask>,
}

impl ChecksExecutor {
    pub fn new(db_service: DbService, github_sender: Option<mpsc::Sender<GitHubTask>>) -> Self {
        let (check_sender, check_receiver) = mpsc::channel(1000);
        let pool = db_service.pool.clone();
        Self {
            check_sender,
            check_receiver: Some(check_receiver),
            db_service,
            github_sender,
            journal: TaskJournal::new(pool, "checks"),
        }
    }

    async fn handle_check_task(&self, task: CheckTask) -> Result<()> {
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

        let path_str = path
            .to_str()
            .with_context(|| format!("checkout path contains non-UTF-8 bytes: {:?}", path))?;

        // Clone the repository (5-minute timeout for large repos)
        let clone_output = tokio::time::timeout(
            std::time::Duration::from_secs(5 * 60),
            Command::new("git")
                .args(["clone", clone_url, path_str])
                .output(),
        )
        .await
        .context("git clone timed out after 5 minutes")?
        .context("failed to execute git clone")?;

        if !clone_output.status.success() {
            anyhow::bail!(
                "git clone failed: {}",
                String::from_utf8_lossy(&clone_output.stderr)
            );
        }

        // Checkout the specific SHA
        debug!("Checking out SHA {} in {:?}", sha, path);
        let checkout_output = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            Command::new("git")
                .current_dir(path)
                .args(["checkout", sha])
                .output(),
        )
        .await
        .context("git checkout timed out after 60 seconds")?
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

impl AsyncService<CheckTask> for ChecksExecutor {
    fn get_sender(&self) -> mpsc::Sender<CheckTask> {
        self.check_sender.clone()
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<CheckTask>> {
        self.check_receiver.take()
    }

    fn task_journal(&self) -> Option<&TaskJournal<CheckTask>> {
        Some(&self.journal)
    }

    async fn handle_task(&self, task: CheckTask) -> Result<()> {
        self.handle_check_task(task).await
    }

    async fn handle_failure(&mut self, error: anyhow::Error) {
        error!(error = %error, "Failed to handle check task");
    }

    async fn handle_closure(&mut self) {
        info!("ChecksExecutor service shutdown gracefully");
    }
}
