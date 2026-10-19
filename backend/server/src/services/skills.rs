use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::ai::AiClient;
use crate::db::DbService;
use crate::github::GitHubTask;
use crate::services::{AsyncService, TaskJournal};
use crate::skills::executor::{assemble_user_content, generate_diff, read_prompt_file};
use crate::skills::types::{SkillResultMessage, SkillTask};

pub struct SkillsExecutor {
    skill_sender: mpsc::Sender<SkillTask>,
    skill_receiver: Option<mpsc::Receiver<SkillTask>>,
    db_service: DbService,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    ai_client: Arc<AiClient>,
    journal: TaskJournal<SkillTask>,
}

impl SkillsExecutor {
    pub fn new(
        db_service: DbService,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        ai_client: Arc<AiClient>,
    ) -> Self {
        let (skill_sender, skill_receiver) = mpsc::channel(1000);
        let pool = db_service.pool.clone();
        Self {
            skill_sender,
            skill_receiver: Some(skill_receiver),
            db_service,
            github_sender,
            ai_client,
            journal: TaskJournal::new(pool, "skills"),
        }
    }

    async fn handle_skill_task(&self, task: SkillTask) -> Result<()> {
        info!(
            "Evaluating skill '{}' for {}/{}@{}",
            task.skill_name, task.owner, task.repo_name, task.sha
        );

        let temp_dir = tempfile::tempdir().context("failed to create temp directory")?;
        let checkout_path = temp_dir.path();

        let clone_url = format!("https://github.com/{}/{}.git", task.owner, task.repo_name);
        clone_and_checkout(&clone_url, &task.sha, &task.base_sha, checkout_path)
            .await
            .context("failed to clone repository")?;

        // Read the prompt file from the checkout
        let system_prompt = read_prompt_file(checkout_path, &task.config.prompt_file)
            .await
            .context("failed to read skill prompt file")?;

        // Generate the diff
        let diff = generate_diff(checkout_path, &task.base_sha, &task.sha)
            .await
            .context("failed to generate diff")?;

        // Assemble user content
        let user_content = assemble_user_content(
            &diff,
            task.pr_title.as_deref(),
            task.pr_description.as_deref(),
            task.pr_author.as_deref(),
        );

        // Resolve model ID
        let model_id = task.config.model.as_deref();
        let model_id_str = model_id.unwrap_or("default").to_string();

        // Call the AI model
        let start = Instant::now();
        let verdict_result = self
            .ai_client
            .evaluate_skill(model_id, &system_prompt, &user_content)
            .await;
        let duration_ms = start.elapsed().as_millis() as i64;

        // Build the result message
        let result_msg = match verdict_result {
            Ok(verdict) => {
                let success = verdict.decision == crate::ai::SkillDecision::Pass;
                info!(
                    "Skill '{}' verdict: {} ({}ms)",
                    task.skill_name, verdict.decision, duration_ms
                );
                SkillResultMessage {
                    skill_name: task.skill_name.clone(),
                    owner: task.owner.clone(),
                    repo_name: task.repo_name.clone(),
                    sha: task.sha.clone(),
                    success,
                    advisory: task.config.advisory,
                    error: false,
                    reason: verdict.reason,
                    findings_json: serde_json::to_string(&verdict.findings)
                        .unwrap_or_else(|_| "[]".to_string()),
                    model_id: model_id_str,
                    duration_ms,
                    check_run_id: 0, // Filled below
                }
            },
            Err(e) => {
                warn!(
                    "Skill '{}' model error ({}ms): {e:#}",
                    task.skill_name, duration_ms
                );
                SkillResultMessage {
                    skill_name: task.skill_name.clone(),
                    owner: task.owner.clone(),
                    repo_name: task.repo_name.clone(),
                    sha: task.sha.clone(),
                    success: false,
                    advisory: task.config.advisory,
                    error: true,
                    reason: format!("Model error: {e:#}"),
                    findings_json: "[]".to_string(),
                    model_id: model_id_str,
                    duration_ms,
                    check_run_id: 0, // Filled below
                }
            },
        };

        // Store result in DB
        let checkset_id = self
            .db_service
            .insert_github_checkset(&task.sha, &task.skill_name, &task.owner, &task.repo_name)
            .await
            .context("failed to insert skill checkset")?;

        self.db_service
            .insert_skill_result(
                checkset_id,
                result_msg.success,
                result_msg.error,
                &result_msg.reason,
                &result_msg.findings_json,
                &result_msg.model_id,
                result_msg.duration_ms,
            )
            .await
            .context("failed to insert skill result")?;

        // Report to GitHub
        if let Some(github_sender) = &self.github_sender {
            if let Some(check_run_info) = self
                .db_service
                .get_check_run_info_by_checkset(&task.owner, &task.repo_name, checkset_id)
                .await
                .context("failed to get check run info")?
            {
                let msg = SkillResultMessage {
                    check_run_id: check_run_info.check_run_id,
                    ..result_msg
                };

                let github_task = if msg.error {
                    GitHubTask::SkillError(msg)
                } else if msg.success {
                    GitHubTask::SkillComplete(msg)
                } else {
                    GitHubTask::SkillFailed(msg)
                };

                github_sender
                    .send(github_task)
                    .await
                    .context("failed to send GitHub task")?;
            } else {
                warn!(
                    "No check_run_id found for skill '{}' in {}/{}",
                    task.skill_name, task.owner, task.repo_name
                );
            }
        }

        Ok(())
    }
}

/// Clone the repository and fetch both head and base SHAs so that
/// `git diff base..head` works.
async fn clone_and_checkout(
    clone_url: &str,
    head_sha: &str,
    base_sha: &str,
    path: &Path,
) -> Result<()> {
    let path_str = path
        .to_str()
        .with_context(|| format!("checkout path contains non-UTF-8 bytes: {path:?}"))?;

    debug!("Cloning {} to {path_str}", clone_url);

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

    // Fetch the base SHA if it's not already available (shallow clones)
    let fetch_output = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        Command::new("git")
            .current_dir(path)
            .args(["fetch", "origin", base_sha])
            .output(),
    )
    .await
    .context("git fetch base timed out")?
    .context("failed to execute git fetch")?;

    if !fetch_output.status.success() {
        debug!(
            "git fetch base SHA warning: {}",
            String::from_utf8_lossy(&fetch_output.stderr)
        );
    }

    // Checkout the head SHA
    let checkout_output = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        Command::new("git")
            .current_dir(path)
            .args(["checkout", head_sha])
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

impl AsyncService<SkillTask> for SkillsExecutor {
    fn get_sender(&self) -> mpsc::Sender<SkillTask> {
        self.skill_sender.clone()
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<SkillTask>> {
        self.skill_receiver.take()
    }

    fn task_journal(&self) -> Option<&TaskJournal<SkillTask>> {
        Some(&self.journal)
    }

    async fn handle_task(&self, task: SkillTask) -> Result<()> {
        self.handle_skill_task(task).await
    }

    async fn handle_failure(&mut self, error: anyhow::Error) {
        error!(error = %error, "Failed to handle skill task");
    }

    async fn handle_closure(&mut self) {
        info!("SkillsExecutor service shutdown gracefully");
    }
}
