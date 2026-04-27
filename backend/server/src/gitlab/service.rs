use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};

use crate::db::DbService;
use crate::gitlab::types::{GitLabCIInfo, GitLabTask};
use crate::graph::GraphServiceHandle;
use crate::metrics::ChangeSummaryMetrics;
use crate::services::AsyncService;

/// GitLabService handles CI integration with GitLab instances
///
/// Unlike GitHub, GitLab uses commit statuses instead of check runs,
/// and posts detailed results as MR comments instead of check annotations.
pub struct GitLabService {
    db_service: DbService,
    gitlab_sender: mpsc::Sender<GitLabTask>,
    gitlab_receiver: Mutex<Option<mpsc::Receiver<GitLabTask>>>,
    /// Tracks configure gate status IDs per commit
    configure_statuses: Mutex<HashMap<String, i64>>,
    /// Tracks eval job status IDs per (commit, job_name)
    eval_statuses: Mutex<HashMap<(String, String), i64>>,
    /// Graph handle for rebuild impact analysis
    graph_handle: GraphServiceHandle,
    /// Optional metrics for observability
    change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
    // TODO: Add GitLab API client when implementing
    // gitlab_client: Arc<GitLabClient>,
}

impl GitLabService {
    pub async fn new(
        db_service: DbService,
        graph_handle: GraphServiceHandle,
        change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
    ) -> Result<Self> {
        let (gitlab_sender, gitlab_receiver) = mpsc::channel(100);

        Ok(Self {
            db_service,
            gitlab_sender,
            gitlab_receiver: Mutex::new(Some(gitlab_receiver)),
            configure_statuses: Mutex::new(HashMap::new()),
            eval_statuses: Mutex::new(HashMap::new()),
            graph_handle,
            change_summary_metrics,
        })
    }

    pub fn get_sender(&self) -> mpsc::Sender<GitLabTask> {
        self.gitlab_sender.clone()
    }

    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<GitLabTask>> {
        self.gitlab_receiver.blocking_lock().take()
    }

    async fn handle_gitlab_task(&self, task: &GitLabTask) -> Result<()> {
        match task {
            GitLabTask::UpdateBuildStatus { drv_id, status } => {
                debug!("GitLab UpdateBuildStatus for {:?}: {:?}", drv_id, status);
                // TODO: Query GitLabCommitStatuses and update via GitLab API
                Ok(())
            },
            GitLabTask::CreateJobSet {
                ci_info,
                name,
                jobs,
                config_json,
            } => {
                self.create_job_set(ci_info, name, jobs, config_json.as_deref())
                    .await
            },
            GitLabTask::CreateCIConfigureGate { ci_info } => {
                debug!("GitLab CreateCIConfigureGate for {}", ci_info.commit);
                // TODO: Post pending commit status via GitLab API
                Ok(())
            },
            GitLabTask::CompleteCIConfigureGate { ci_info } => {
                debug!("GitLab CompleteCIConfigureGate for {}", ci_info.commit);
                // TODO: Update status to success via GitLab API
                self.configure_statuses.lock().await.remove(&ci_info.commit);
                Ok(())
            },
            GitLabTask::CreateChangeSummaryComment { ci_info, job } => {
                self.handle_create_change_summary_comment(ci_info, job)
                    .await
            },
            // Other tasks are stubs for now
            _ => {
                debug!("GitLab task not yet implemented: {:?}", task);
                Ok(())
            },
        }
    }

    async fn create_job_set(
        &self,
        ci_info: &Arc<GitLabCIInfo>,
        name: &str,
        jobs: &[crate::nix::nix_eval_jobs::NixEvalDrv],
        config_json: Option<&str>,
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
        .fetch_one(&self.db_service.pool)
        .await?;

        // Create jobs for this jobset (platform-agnostic logic)
        // Reuse GitHub's create_jobs_for_jobset implementation
        crate::db::github::create_jobs_for_jobset(jobset_id, jobs, &self.db_service.pool).await?;

        // TODO: If this is a PR head, schedule change summary
        // For now, just log success
        info!(
            "Created GitLab jobset {} for {}/{}@{} (job: {})",
            jobset_id, ci_info.owner, ci_info.repo_name, ci_info.commit, name
        );

        Ok(())
    }

    /// Post (or update) change summary as an MR comment
    async fn handle_create_change_summary_comment(
        &self,
        ci_info: &Arc<GitLabCIInfo>,
        job: &str,
    ) -> Result<()> {
        let Some(base_sha) = ci_info.base_commit.as_deref() else {
            debug!(
                "Skipping change-summary for {}: no base commit (not an MR head)",
                &ci_info.commit
            );
            return Ok(());
        };

        // Resolve head jobset ID from GitLabJobSets
        let head_jobset: Option<i64> = sqlx::query_scalar(
            "SELECT ROWID FROM GitLabJobSets WHERE sha = ? AND job = ? AND domain = ?",
        )
        .bind(&ci_info.commit)
        .bind(job)
        .bind(&ci_info.domain)
        .fetch_optional(&self.db_service.pool)
        .await?;

        let Some(head_jobset_id) = head_jobset else {
            debug!(
                "No head jobset for sha={} job={} domain={}; skipping change-summary",
                &ci_info.commit, job, &ci_info.domain
            );
            return Ok(());
        };

        // Resolve base jobset ID if it exists
        let base_jobset_id: Option<i64> = sqlx::query_scalar(
            "SELECT ROWID FROM GitLabJobSets WHERE sha = ? AND job = ? AND domain = ?",
        )
        .bind(base_sha)
        .bind(job)
        .bind(&ci_info.domain)
        .fetch_optional(&self.db_service.pool)
        .await?;

        // Create JobsetData for the head commit
        let jobset_data = crate::jobset_data::JobsetData::new(
            &ci_info.owner,
            &ci_info.repo_name,
            &ci_info.domain,
            &ci_info.commit,
            job,
            None,
        );

        // Resolve options from jobset data
        let (opts, status) = crate::change_summary::resolve_options_from_jobset_data(
            &jobset_data,
            self.change_summary_metrics.as_deref(),
        )
        .await;

        // Build change summary using platform-agnostic function
        let summary = match crate::change_summary::build_change_summary_from_jobset_ids(
            &self.db_service.pool,
            &self.graph_handle,
            head_jobset_id,
            base_jobset_id,
            &jobset_data,
            base_sha,
            &opts,
            &status,
            self.change_summary_metrics.as_deref(),
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    "Failed to build change-summary for commit {}: {:?}",
                    &ci_info.commit, e
                );
                return Ok(());
            },
        };

        let markdown = summary.markdown;

        // TODO: Post markdown as MR comment via GitLab API
        // For now, just log that we would post it
        info!(
            "Would post change-summary comment for MR in {}/{} (project {})",
            ci_info.owner, ci_info.repo_name, ci_info.project_id
        );
        debug!("Change summary markdown:\n{}", markdown);

        Ok(())
    }
}

// ============================================================================
// AsyncService trait implementation
// ============================================================================

impl AsyncService<GitLabTask> for GitLabService {
    fn get_sender(&self) -> mpsc::Sender<GitLabTask> {
        self.gitlab_sender.clone()
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<GitLabTask>> {
        self.gitlab_receiver.blocking_lock().take()
    }

    async fn handle_task(&self, task: GitLabTask) -> Result<()> {
        self.handle_gitlab_task(&task).await
    }

    async fn handle_failure(&mut self, error: anyhow::Error) {
        error!("GitLabService task failed: {:?}", error);
    }

    async fn handle_closure(&mut self) {
        info!("GitLabService shutting down");
    }
}
