use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};

use crate::db::DbService;
use crate::gitlab::GitLabClient;
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
    /// GitLab API clients per domain (self-hosted instances)
    gitlab_clients: Mutex<HashMap<String, Arc<GitLabClient>>>,
}

impl GitLabService {
    pub async fn new(
        db_service: DbService,
        graph_handle: GraphServiceHandle,
        change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
        gitlab_configs: &HashMap<String, crate::config::GitLabInstanceConfig>,
    ) -> Result<Self> {
        let (gitlab_sender, gitlab_receiver) = mpsc::channel(100);

        // Initialize GitLab clients from configuration
        let mut gitlab_clients = HashMap::new();

        for (domain, config) in gitlab_configs {
            match GitLabClient::new(domain, config.token.expose().to_string()).await {
                Ok(client) => {
                    info!("Initialized GitLab client for domain: {}", domain);
                    gitlab_clients.insert(domain.clone(), Arc::new(client));
                },
                Err(e) => {
                    warn!("Failed to initialize GitLab client for {}: {:?}", domain, e);
                },
            }
        }

        if gitlab_clients.is_empty() {
            info!("GitLab integration disabled (no instances configured)");
        } else {
            info!(
                "GitLab integration enabled for {} instance(s)",
                gitlab_clients.len()
            );
        }

        Ok(Self {
            db_service,
            gitlab_sender,
            gitlab_receiver: Mutex::new(Some(gitlab_receiver)),
            configure_statuses: Mutex::new(HashMap::new()),
            eval_statuses: Mutex::new(HashMap::new()),
            graph_handle,
            change_summary_metrics,
            gitlab_clients: Mutex::new(gitlab_clients),
        })
    }

    pub fn get_sender(&self) -> mpsc::Sender<GitLabTask> {
        self.gitlab_sender.clone()
    }

    #[allow(dead_code)] // Called via AsyncService trait dispatch
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<GitLabTask>> {
        self.gitlab_receiver.blocking_lock().take()
    }

    /// Get the GitLab client for a specific domain
    async fn get_client(&self, domain: &str) -> Option<Arc<GitLabClient>> {
        self.gitlab_clients.lock().await.get(domain).cloned()
    }

    async fn handle_gitlab_task(&self, task: &GitLabTask) -> Result<()> {
        match task {
            GitLabTask::UpdateBuildStatus { drv_id, status } => {
                self.handle_update_build_status(drv_id, status).await
            },
            GitLabTask::UpdateBuildStatusWithSizeWarning {
                drv_id,
                status,
                baseline_size,
                current_size,
                increase_percent,
                threshold_percent: _,
            } => {
                self.handle_update_build_status_with_size_warning(
                    drv_id,
                    status,
                    *baseline_size,
                    *current_size,
                    *increase_percent,
                )
                .await
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
                self.handle_create_ci_configure_gate(ci_info).await
            },
            GitLabTask::CompleteCIConfigureGate { ci_info } => {
                self.handle_complete_ci_configure_gate(ci_info).await
            },
            GitLabTask::CreateFailureStatus {
                drv_id,
                jobset_id,
                job_attr_name,
                difference,
            } => {
                self.handle_create_failure_status(drv_id, *jobset_id, job_attr_name, difference)
                    .await
            },
            GitLabTask::CancelStatusesForCommit { ci_info } => {
                self.handle_cancel_statuses_for_commit(ci_info).await
            },
            GitLabTask::CreateCIEvalJob { ci_info, job_title } => {
                self.handle_create_ci_eval_job(ci_info, job_title).await
            },
            GitLabTask::CompleteCIEvalJob {
                ci_info,
                job_name,
                success,
            } => {
                self.handle_complete_ci_eval_job(ci_info, job_name, *success)
                    .await
            },
            GitLabTask::FailCIEvalJob {
                ci_info,
                job_name,
                errors,
            } => {
                self.handle_fail_ci_eval_job(ci_info, job_name, errors)
                    .await
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

        // Look up the MR by head SHA to get the MR IID
        let mr = match crate::db::gitlab::get_mr_by_head_sha(
            &ci_info.commit,
            ci_info.project_id,
            &self.db_service.pool,
        )
        .await
        {
            Ok(Some(mr)) => mr,
            Ok(None) => {
                debug!(
                    "No MR found for commit {} in project {}; skipping change-summary comment",
                    &ci_info.commit, ci_info.project_id
                );
                return Ok(());
            },
            Err(e) => {
                warn!(
                    "Failed to look up MR for commit {}: {:?}",
                    &ci_info.commit, e
                );
                return Ok(());
            },
        };

        // Get GitLab client for this domain
        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No GitLab client for domain {}", ci_info.domain);
            return Ok(());
        };

        // Post or update the sticky change summary comment
        let marker = format!("<!-- eka-ci-change-summary-{} -->", job);
        match client
            .post_or_update_sticky_comment(ci_info.project_id, mr.mr_iid, &marker, &markdown)
            .await
        {
            Ok(_) => {
                info!(
                    "Posted change-summary comment for MR !{} in {}/{} (project {})",
                    mr.mr_iid, ci_info.owner, ci_info.repo_name, ci_info.project_id
                );
            },
            Err(e) => {
                warn!(
                    "Failed to post change-summary comment for MR !{}: {:?}",
                    mr.mr_iid, e
                );
            },
        }

        Ok(())
    }

    /// Create the initial CI configure gate commit status
    async fn handle_create_ci_configure_gate(&self, ci_info: &Arc<GitLabCIInfo>) -> Result<()> {
        debug!(
            "Creating CI configure gate status for commit {} in project {}",
            ci_info.commit, ci_info.project_id
        );

        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No GitLab client for domain {}", ci_info.domain);
            return Ok(());
        };

        match crate::gitlab::actions::create_ci_configure_gate(&client, ci_info).await {
            Ok(status_id) => {
                // Store the status ID for later updates
                self.configure_statuses
                    .lock()
                    .await
                    .insert(ci_info.commit.clone(), status_id);
                info!(
                    "Created CI configure gate status {} for commit {}",
                    status_id, ci_info.commit
                );
            },
            Err(e) => {
                warn!(
                    "Failed to create CI configure gate status for commit {}: {:?}",
                    ci_info.commit, e
                );
            },
        }

        Ok(())
    }

    /// Complete the CI configure gate commit status
    async fn handle_complete_ci_configure_gate(&self, ci_info: &Arc<GitLabCIInfo>) -> Result<()> {
        debug!(
            "Completing CI configure gate status for commit {} in project {}",
            ci_info.commit, ci_info.project_id
        );

        // Remove from tracking map
        self.configure_statuses.lock().await.remove(&ci_info.commit);

        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No GitLab client for domain {}", ci_info.domain);
            return Ok(());
        };

        match crate::gitlab::actions::update_ci_configure_gate(&client, ci_info).await {
            Ok(()) => {
                info!(
                    "Successfully completed CI configure gate status for commit {}",
                    ci_info.commit
                );
            },
            Err(e) => {
                warn!(
                    "Failed to complete CI configure gate status for commit {}: {:?}",
                    ci_info.commit, e
                );
            },
        }

        Ok(())
    }

    /// Update the status of a build in GitLab
    async fn handle_update_build_status(
        &self,
        drv_id: &crate::db::model::DrvId,
        status: &crate::db::model::build_event::DrvBuildState,
    ) -> Result<()> {
        debug!(
            "Updating GitLab build status for {:?}: {:?}",
            drv_id, status
        );

        // Query all commit statuses associated with this derivation
        let statuses =
            crate::db::gitlab::commit_statuses_for_drv_path(drv_id, &self.db_service.pool).await?;

        if statuses.is_empty() {
            debug!("No commit statuses found for drv {:?}", drv_id);
            return Ok(());
        }

        // Update each status
        for commit_status in statuses {
            let Some(client) = self.get_client(&commit_status.domain).await else {
                warn!("No GitLab client for domain {}", commit_status.domain);
                continue;
            };

            // Use the actions module to update the status
            if let Err(e) = crate::gitlab::actions::update_build_status(
                &client,
                commit_status.project_id,
                &commit_status.sha,
                &commit_status.name,
                status,
            )
            .await
            {
                warn!(
                    "Failed to update GitLab status {} for drv {:?}: {:?}",
                    commit_status.status_id, drv_id, e
                );
            } else {
                debug!(
                    "Successfully updated GitLab status {} for drv {:?}",
                    commit_status.status_id, drv_id
                );

                // Update our local database tracking
                let state: crate::gitlab::types::GitLabStatusState = status.clone().into();
                let state_str = format!("{:?}", state).to_lowercase();
                if let Err(e) = crate::db::gitlab::update_commit_status_state(
                    commit_status.status_id,
                    &state_str,
                    &self.db_service.pool,
                )
                .await
                {
                    warn!("Failed to update commit status state in database: {:?}", e);
                }
            }
        }

        Ok(())
    }

    /// Update build status with a size warning
    async fn handle_update_build_status_with_size_warning(
        &self,
        drv_id: &crate::db::model::DrvId,
        status: &crate::db::model::build_event::DrvBuildState,
        baseline_size: u64,
        current_size: u64,
        increase_percent: f64,
    ) -> Result<()> {
        debug!(
            "Updating GitLab build status with size warning for {:?}: {:?}",
            drv_id, status
        );

        // Query all commit statuses associated with this derivation
        let statuses =
            crate::db::gitlab::commit_statuses_for_drv_path(drv_id, &self.db_service.pool).await?;

        if statuses.is_empty() {
            debug!("No commit statuses found for drv {:?}", drv_id);
            return Ok(());
        }

        // Update each status with size warning
        for commit_status in statuses {
            let Some(client) = self.get_client(&commit_status.domain).await else {
                warn!("No GitLab client for domain {}", commit_status.domain);
                continue;
            };

            if let Err(e) = crate::gitlab::actions::update_status_with_size_warning(
                &client,
                commit_status.project_id,
                &commit_status.sha,
                &commit_status.name,
                baseline_size,
                current_size,
                increase_percent,
            )
            .await
            {
                warn!(
                    "Failed to update GitLab status with size warning for {:?}: {:?}",
                    drv_id, e
                );
            } else {
                debug!(
                    "Successfully updated GitLab status with size warning for {:?}",
                    drv_id
                );
            }
        }

        Ok(())
    }

    /// Create a failure status for a build
    async fn handle_create_failure_status(
        &self,
        drv_id: &crate::db::model::DrvId,
        jobset_id: i64,
        job_attr_name: &str,
        _difference: &crate::github::JobDifference,
    ) -> Result<()> {
        debug!(
            "Creating failure status for drv {:?}, jobset {}, job {}",
            drv_id, jobset_id, job_attr_name
        );

        // Query the jobset to get CI info
        let jobset: Option<(String, i64, String, String, String, i64)> = sqlx::query_as(
            r#"
            SELECT sha, project_id, owner, repo_name, domain, project_id
            FROM GitLabJobSets
            WHERE ROWID = ?
            "#,
        )
        .bind(jobset_id)
        .fetch_optional(&self.db_service.pool)
        .await?;

        let Some((sha, project_id, owner, repo_name, domain, _)) = jobset else {
            warn!("No jobset found with id {}", jobset_id);
            return Ok(());
        };

        let Some(client) = self.get_client(&domain).await else {
            warn!("No GitLab client for domain {}", domain);
            return Ok(());
        };

        // Get the drv ROWID for database storage
        let drv_rowid: Option<i64> = sqlx::query_scalar("SELECT ROWID FROM Drv WHERE drv_path = ?")
            .bind(drv_id)
            .fetch_optional(&self.db_service.pool)
            .await?;

        let Some(drv_rowid) = drv_rowid else {
            warn!("No drv found for path {:?}", drv_id);
            return Ok(());
        };

        let status_name = format!("eka-ci/{}", job_attr_name);
        let description = "Build failed";

        match crate::gitlab::actions::create_failure_status(
            &client,
            project_id,
            &sha,
            &status_name,
            description,
        )
        .await
        {
            Ok(status_id) => {
                debug!("Created failure status {} for drv {:?}", status_id, drv_id);

                // Store in database
                if let Err(e) = crate::db::gitlab::insert_commit_status_info(
                    status_id,
                    &sha,
                    &status_name,
                    project_id,
                    &domain,
                    &owner,
                    &repo_name,
                    drv_rowid,
                    "failed",
                    &self.db_service.pool,
                )
                .await
                {
                    warn!("Failed to insert commit status into database: {:?}", e);
                }
            },
            Err(e) => {
                warn!(
                    "Failed to create failure status for drv {:?}: {:?}",
                    drv_id, e
                );
            },
        }

        Ok(())
    }

    /// Cancel all statuses for a commit
    async fn handle_cancel_statuses_for_commit(&self, ci_info: &GitLabCIInfo) -> Result<()> {
        debug!(
            "Canceling all statuses for commit {} in project {}",
            ci_info.commit, ci_info.project_id
        );

        // Query all active statuses for this commit
        let statuses =
            crate::db::gitlab::commit_statuses_for_commit(&ci_info.commit, &self.db_service.pool)
                .await?;

        if statuses.is_empty() {
            debug!("No active statuses found for commit {}", ci_info.commit);
            return Ok(());
        }

        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No GitLab client for domain {}", ci_info.domain);
            return Ok(());
        };

        // Cancel each status
        for status in statuses {
            if let Err(e) = crate::gitlab::actions::cancel_commit_status(
                &client,
                status.project_id,
                &status.sha,
                &status.name,
            )
            .await
            {
                warn!(
                    "Failed to cancel status {} for commit {}: {:?}",
                    status.status_id, ci_info.commit, e
                );
            } else {
                debug!(
                    "Successfully canceled status {} for commit {}",
                    status.status_id, ci_info.commit
                );

                // Update database
                if let Err(e) = crate::db::gitlab::update_commit_status_state(
                    status.status_id,
                    "canceled",
                    &self.db_service.pool,
                )
                .await
                {
                    warn!("Failed to update status state in database: {:?}", e);
                }
            }
        }

        Ok(())
    }

    /// Create a CI eval job status
    async fn handle_create_ci_eval_job(
        &self,
        ci_info: &GitLabCIInfo,
        job_title: &str,
    ) -> Result<()> {
        debug!(
            "Creating CI eval job status for job '{}' on commit {}",
            job_title, ci_info.commit
        );

        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No GitLab client for domain {}", ci_info.domain);
            return Ok(());
        };

        match crate::gitlab::actions::create_ci_eval_job(&client, ci_info, job_title).await {
            Ok(status_id) => {
                debug!(
                    "Created eval job status {} for job '{}' on commit {}",
                    status_id, job_title, ci_info.commit
                );

                // Store in eval_statuses HashMap for later updates
                self.eval_statuses
                    .lock()
                    .await
                    .insert((ci_info.commit.clone(), job_title.to_string()), status_id);

                info!(
                    "Created CI eval job status for job '{}' on commit {}",
                    job_title, ci_info.commit
                );
            },
            Err(e) => {
                warn!(
                    "Failed to create CI eval job status for job '{}' on commit {}: {:?}",
                    job_title, ci_info.commit, e
                );
            },
        }

        Ok(())
    }

    /// Complete a CI eval job status
    async fn handle_complete_ci_eval_job(
        &self,
        ci_info: &GitLabCIInfo,
        job_name: &str,
        success: bool,
    ) -> Result<()> {
        debug!(
            "Completing CI eval job status for job '{}' with success={} on commit {}",
            job_name, success, ci_info.commit
        );

        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No GitLab client for domain {}", ci_info.domain);
            return Ok(());
        };

        // Remove from tracking map
        self.eval_statuses
            .lock()
            .await
            .remove(&(ci_info.commit.clone(), job_name.to_string()));

        match crate::gitlab::actions::update_ci_eval_job(&client, ci_info, job_name, success).await
        {
            Ok(()) => {
                info!(
                    "Completed CI eval job status for job '{}' on commit {} (success={})",
                    job_name, ci_info.commit, success
                );
            },
            Err(e) => {
                warn!(
                    "Failed to complete CI eval job status for job '{}' on commit {}: {:?}",
                    job_name, ci_info.commit, e
                );
            },
        }

        Ok(())
    }

    /// Create a failed CI eval job status with error details
    async fn handle_fail_ci_eval_job(
        &self,
        ci_info: &GitLabCIInfo,
        job_name: &str,
        errors: &[crate::nix::nix_eval_jobs::NixEvalError],
    ) -> Result<()> {
        debug!(
            "Creating failed CI eval job status for job '{}' on commit {} with {} errors",
            job_name,
            ci_info.commit,
            errors.len()
        );

        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No GitLab client for domain {}", ci_info.domain);
            return Ok(());
        };

        match crate::gitlab::actions::fail_ci_eval_job(&client, ci_info, job_name, errors).await {
            Ok(status_id) => {
                info!(
                    "Created failed CI eval job status {} for job '{}' on commit {} ({} errors)",
                    status_id,
                    job_name,
                    ci_info.commit,
                    errors.len()
                );
            },
            Err(e) => {
                warn!(
                    "Failed to create failed CI eval job status for job '{}' on commit {}: {:?}",
                    job_name, ci_info.commit, e
                );
            },
        }

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
