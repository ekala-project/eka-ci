// Jobset creation for GitLab

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};

use crate::gitlab::types::{GitLabCIInfo, GitLabTask};

/// Debounce delay for change summary generation (5 minutes)
/// This allows multiple jobsets to be created for the same commit
/// before triggering a single change summary
pub const CHANGE_SUMMARY_DEBOUNCE: Duration = Duration::from_secs(5 * 60);

/// Create a new jobset in the GitLabJobSets table
pub(super) async fn create_job_set(
    ci_info: &Arc<GitLabCIInfo>,
    name: &str,
    jobs: &[crate::nix::nix_eval_jobs::NixEvalDrv],
    config_json: Option<&str>,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    gitlab_sender: &mpsc::Sender<GitLabTask>,
    change_summary_pending: &Mutex<HashSet<String>>,
) -> Result<()> {
    // Insert into GitLabJobSets table
    let jobset_id: i64 = sqlx::query_scalar(
        r#"
        INSERT INTO GitLabJobSets (sha, job, owner, repo_name, project_id, domain, config_json)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        RETURNING ROWID
        "#,
    )
    .bind(&ci_info.commit)
    .bind(name)
    .bind(&ci_info.owner)
    .bind(&ci_info.repo_name)
    .bind(ci_info.project_id)
    .bind(&ci_info.domain)
    .bind(config_json)
    .fetch_one(db_pool)
    .await?;

    // Create jobs for this jobset (platform-agnostic logic)
    // Reuse GitHub's create_jobs_for_jobset implementation
    crate::db::github::create_jobs_for_jobset(jobset_id, jobs, None, db_pool).await?;

    info!(
        "Created GitLab jobset {} for {}/{}@{} (job: {})",
        jobset_id, ci_info.owner, ci_info.repo_name, ci_info.commit, name
    );

    // Schedule change summary for MR heads (commits with base_commit set)
    if ci_info.base_commit.is_some() {
        // Dedup: only schedule one change-summary per commit SHA
        if change_summary_pending
            .lock()
            .await
            .insert(ci_info.commit.clone())
        {
            spawn_change_summary_debounce(gitlab_sender, Arc::clone(ci_info), name.to_string());
        }
    }

    Ok(())
}

/// Spawn the debounce timer that enqueues a `CreateChangeSummaryComment`.
fn spawn_change_summary_debounce(
    gitlab_sender: &mpsc::Sender<GitLabTask>,
    ci_info: Arc<GitLabCIInfo>,
    job: String,
) {
    let sender = gitlab_sender.clone();
    tokio::spawn(async move {
        tokio::time::sleep(CHANGE_SUMMARY_DEBOUNCE).await;
        if let Err(e) = sender
            .send(GitLabTask::CreateChangeSummaryComment { ci_info, job })
            .await
        {
            warn!(
                "Failed to enqueue CreateChangeSummaryComment after debounce: {:?}",
                e
            );
        }
    });
}
