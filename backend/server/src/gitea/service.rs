use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};

use crate::db::DbService;
use crate::gitea::GiteaClient;
use crate::gitea::types::{GiteaCIInfo, GiteaTask};
use crate::graph::GraphServiceHandle;
use crate::metrics::ChangeSummaryMetrics;
use crate::services::AsyncService;

/// GiteaService handles CI integration with Gitea instances
///
/// Gitea uses a GitHub-compatible API, so newer instances support check runs
/// while older instances fall back to commit statuses. The service adapts
/// based on the instance's capabilities.
pub struct GiteaService {
    db_service: DbService,
    gitea_sender: mpsc::Sender<GiteaTask>,
    gitea_receiver: Option<mpsc::Receiver<GiteaTask>>,
    /// Tracks configure gate check run IDs per commit
    configure_checks: Mutex<HashMap<String, i64>>,
    /// Tracks eval job check run IDs per (commit, job_name)
    eval_checks: Mutex<HashMap<(String, String), i64>>,
    /// Tracks change-summary check run IDs per commit
    #[allow(dead_code)]
    change_summary_checks: Mutex<HashMap<String, i64>>,
    /// Graph handle for rebuild impact analysis
    graph_handle: GraphServiceHandle,
    /// Optional metrics for observability
    change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
    /// Gitea API clients per domain (self-hosted instances)
    gitea_clients: Mutex<HashMap<String, Arc<GiteaClient>>>,
}

impl GiteaService {
    pub async fn new(
        db_service: DbService,
        graph_handle: GraphServiceHandle,
        change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
        gitea_configs: &HashMap<String, crate::config::GiteaInstanceConfig>,
    ) -> Result<Self> {
        let (gitea_sender, gitea_receiver) = mpsc::channel(100);

        // Initialize Gitea clients from configuration
        let mut gitea_clients = HashMap::new();

        for (domain, config) in gitea_configs {
            match GiteaClient::new(domain, config.token.expose().to_string()).await {
                Ok(client) => {
                    info!(
                        "Initialized Gitea client for domain: {} (version: {})",
                        domain,
                        client.version()
                    );
                    gitea_clients.insert(domain.clone(), Arc::new(client));
                },
                Err(e) => {
                    warn!("Failed to initialize Gitea client for {}: {:?}", domain, e);
                },
            }
        }

        if gitea_clients.is_empty() {
            info!("Gitea integration disabled (no instances configured)");
        } else {
            info!(
                "Gitea integration enabled for {} instance(s)",
                gitea_clients.len()
            );
        }

        Ok(Self {
            db_service,
            gitea_sender,
            gitea_receiver: Some(gitea_receiver),
            configure_checks: Mutex::new(HashMap::new()),
            eval_checks: Mutex::new(HashMap::new()),
            change_summary_checks: Mutex::new(HashMap::new()),
            graph_handle,
            change_summary_metrics,
            gitea_clients: Mutex::new(gitea_clients),
        })
    }

    pub fn get_sender(&self) -> mpsc::Sender<GiteaTask> {
        self.gitea_sender.clone()
    }

    #[allow(dead_code)] // Called via AsyncService trait dispatch
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<GiteaTask>> {
        self.gitea_receiver.take()
    }

    /// Get the Gitea client for a specific domain
    async fn get_client(&self, domain: &str) -> Option<Arc<GiteaClient>> {
        self.gitea_clients.lock().await.get(domain).cloned()
    }

    async fn handle_gitea_task(&self, task: &GiteaTask) -> Result<()> {
        match task {
            GiteaTask::UpdateBuildStatus { drv_id, status } => {
                self.handle_update_build_status(drv_id, status).await
            },
            GiteaTask::UpdateBuildStatusWithSizeWarning {
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
            GiteaTask::CreateJobSet {
                ci_info,
                name,
                jobs,
                config_json,
            } => {
                self.create_job_set(ci_info, name, jobs, config_json.as_deref())
                    .await
            },
            GiteaTask::CreateCIConfigureGate { ci_info } => {
                self.handle_create_ci_configure_gate(ci_info).await
            },
            GiteaTask::CompleteCIConfigureGate { ci_info } => {
                self.handle_complete_ci_configure_gate(ci_info).await
            },
            GiteaTask::CreateCIEvalJob { ci_info, job_title } => {
                self.handle_create_ci_eval_job(ci_info, job_title).await
            },
            GiteaTask::CompleteCIEvalJob {
                ci_info,
                job_name,
                conclusion,
            } => {
                self.handle_complete_ci_eval_job(ci_info, job_name, conclusion)
                    .await
            },
            GiteaTask::FailCIEvalJob {
                ci_info,
                job_name,
                errors,
            } => {
                self.handle_fail_ci_eval_job(ci_info, job_name, errors)
                    .await
            },
            GiteaTask::CancelCheckRunsForCommit { ci_info } => {
                self.handle_cancel_check_runs_for_commit(ci_info).await
            },
            GiteaTask::CreateFailureCheckRun {
                drv_id,
                jobset_id,
                job_attr_name,
                difference,
            } => {
                self.handle_create_failure_check_run(drv_id, *jobset_id, job_attr_name, difference)
                    .await
            },
            GiteaTask::CreateChangeSummaryCheck { ci_info, job } => {
                self.handle_create_change_summary_check(ci_info, job).await
            },
            GiteaTask::CheckAutoMerge {
                domain,
                owner,
                repo_name,
                pr_number,
            } => {
                self.handle_check_auto_merge(domain, owner, repo_name, *pr_number)
                    .await
            },
            GiteaTask::ProcessMergeCommand {
                domain,
                owner,
                repo_name,
                pr_number,
                comment_id,
                requester_id,
                requester_login,
                body,
                comment_created_at,
            } => {
                self.handle_process_merge_command(
                    domain,
                    owner,
                    repo_name,
                    *pr_number,
                    *comment_id,
                    *requester_id,
                    requester_login,
                    body,
                    comment_created_at,
                )
                .await
            },
            GiteaTask::CommentMergeDriftCancelled {
                domain,
                owner,
                repo_name,
                pr_number,
                expected_sha,
                actual_sha,
                requester_login,
            } => {
                self.handle_comment_merge_drift_cancelled(
                    domain,
                    owner,
                    repo_name,
                    *pr_number,
                    expected_sha,
                    actual_sha,
                    requester_login,
                )
                .await
            },
            GiteaTask::CreateDependencyChangesGate {
                ci_info,
                jobset_id,
                base_jobset_id,
            } => {
                self.handle_create_dependency_changes_gate(ci_info, *jobset_id, *base_jobset_id)
                    .await
            },
        }
    }

    async fn create_job_set(
        &self,
        ci_info: &Arc<GiteaCIInfo>,
        name: &str,
        jobs: &[crate::nix::nix_eval_jobs::NixEvalDrv],
        config_json: Option<&str>,
    ) -> Result<()> {
        // Insert into GiteaJobSets table
        let jobset_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO GiteaJobSets (sha, job, owner, repo_name, domain, config_json)
            VALUES (?, ?, ?, ?, ?, ?)
            RETURNING ROWID
            "#,
        )
        .bind(&ci_info.commit)
        .bind(name)
        .bind(&ci_info.owner)
        .bind(&ci_info.repo_name)
        .bind(&ci_info.domain)
        .bind(config_json)
        .fetch_one(&self.db_service.pool)
        .await?;

        // Create jobs for this jobset (platform-agnostic logic)
        // Reuse the same function as GitHub/GitLab
        crate::db::github::create_jobs_for_jobset(jobset_id, jobs, None, &self.db_service.pool)
            .await?;

        // TODO: If this is a PR head, schedule change summary
        info!(
            "Created Gitea jobset {} for {}/{}/{}@{} (job: {})",
            jobset_id, ci_info.domain, ci_info.owner, ci_info.repo_name, ci_info.commit, name
        );

        Ok(())
    }

    /// Post (or update) change summary check for a PR head
    /// Uses check runs API for newer Gitea versions, commit statuses for older
    async fn handle_create_change_summary_check(
        &self,
        ci_info: &Arc<GiteaCIInfo>,
        job: &str,
    ) -> Result<()> {
        let Some(base_sha) = ci_info.base_commit.as_deref() else {
            debug!(
                "Skipping change-summary for {}: no base commit (not a PR head)",
                &ci_info.commit
            );
            return Ok(());
        };

        // Resolve head jobset ID from GiteaJobSets
        let head_jobset: Option<i64> = sqlx::query_scalar(
            "SELECT ROWID FROM GiteaJobSets WHERE sha = ? AND job = ? AND domain = ?",
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
            "SELECT ROWID FROM GiteaJobSets WHERE sha = ? AND job = ? AND domain = ?",
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

        // Look up the PR by head SHA to get the PR number
        let pr = match crate::db::gitea::get_pr_by_head_sha(
            &ci_info.commit,
            &ci_info.domain,
            &ci_info.owner,
            &ci_info.repo_name,
            &self.db_service.pool,
        )
        .await
        {
            Ok(Some(pr)) => pr,
            Ok(None) => {
                debug!(
                    "No PR found for commit {} in {}/{}/{}; skipping change-summary",
                    &ci_info.commit, ci_info.domain, ci_info.owner, ci_info.repo_name
                );
                return Ok(());
            },
            Err(e) => {
                warn!(
                    "Failed to look up PR for commit {}: {:?}",
                    &ci_info.commit, e
                );
                return Ok(());
            },
        };

        // Get Gitea client for this domain
        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No Gitea client for domain {}", ci_info.domain);
            return Ok(());
        };

        // For Gitea, we can post the change summary as a PR comment
        // Similar to GitLab, we use a marker to identify and update our comment
        let marker = format!("<!-- eka-ci-change-summary-{} -->", job);
        let comment_body = format!("{}\n\n{}", marker, markdown);

        // Post as a new comment (Gitea doesn't have built-in sticky comments like GitLab)
        // In the future, we could search for existing comments and update them
        match client
            .create_issue_comment(
                &ci_info.owner,
                &ci_info.repo_name,
                pr.pr_number,
                &comment_body,
            )
            .await
        {
            Ok(_) => {
                info!(
                    "Posted change-summary comment for PR #{} in {}/{}/{} (domain: {})",
                    pr.pr_number, ci_info.domain, ci_info.owner, ci_info.repo_name, ci_info.domain
                );
            },
            Err(e) => {
                warn!(
                    "Failed to post change-summary comment for PR #{}: {:?}",
                    pr.pr_number, e
                );
            },
        }

        Ok(())
    }

    /// Create the initial CI configure gate check for a commit
    /// Uses check runs for newer Gitea, commit status for older
    async fn handle_create_ci_configure_gate(&self, ci_info: &Arc<GiteaCIInfo>) -> Result<()> {
        debug!(
            "Creating CI configure gate for commit {} in {}/{}",
            ci_info.commit, ci_info.owner, ci_info.repo_name
        );

        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No Gitea client for domain {}", ci_info.domain);
            return Ok(());
        };

        if client.supports_check_runs() {
            // Use Check Runs API via actions module
            match crate::gitea::actions::create_ci_configure_gate(&client, ci_info).await {
                Ok(check_run_id) => {
                    self.configure_checks
                        .lock()
                        .await
                        .insert(ci_info.commit.clone(), check_run_id);
                    info!(
                        "Created CI configure gate check run {} for commit {}",
                        check_run_id, ci_info.commit
                    );
                },
                Err(e) => {
                    warn!(
                        "Failed to create CI configure gate for commit {}: {:?}",
                        ci_info.commit, e
                    );
                },
            }
        } else {
            // Fallback to Commit Status API for older Gitea
            let request = crate::gitea::client::CreateCommitStatusRequest {
                state: crate::gitea::client::CommitStatusState::Pending,
                target_url: None,
                description: Some("Reading repository configuration...".to_string()),
                context: "EkaCI: Configure".to_string(),
            };

            match client
                .create_commit_status(&ci_info.owner, &ci_info.repo_name, &ci_info.commit, request)
                .await
            {
                Ok(_) => {
                    info!(
                        "Created configure gate commit status for commit {}",
                        ci_info.commit
                    );
                },
                Err(e) => {
                    warn!(
                        "Failed to create configure gate commit status for {}: {:?}",
                        ci_info.commit, e
                    );
                },
            }
        }

        Ok(())
    }

    /// Complete the CI configure gate check
    async fn handle_complete_ci_configure_gate(&self, ci_info: &Arc<GiteaCIInfo>) -> Result<()> {
        debug!(
            "Completing CI configure gate for commit {} in {}/{}",
            ci_info.commit, ci_info.owner, ci_info.repo_name
        );

        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No Gitea client for domain {}", ci_info.domain);
            return Ok(());
        };

        if client.supports_check_runs() {
            // Update check run to success via actions module
            if let Some(check_run_id) = self.configure_checks.lock().await.remove(&ci_info.commit) {
                match crate::gitea::actions::update_ci_configure_gate(
                    &client,
                    ci_info,
                    check_run_id,
                )
                .await
                {
                    Ok(()) => {
                        info!(
                            "Successfully completed CI configure gate check run {} for commit {}",
                            check_run_id, ci_info.commit
                        );
                    },
                    Err(e) => {
                        warn!(
                            "Failed to complete CI configure gate for commit {}: {:?}",
                            ci_info.commit, e
                        );
                    },
                }
            }
        } else {
            // Update commit status to success for older Gitea
            let request = crate::gitea::client::CreateCommitStatusRequest {
                state: crate::gitea::client::CommitStatusState::Success,
                target_url: None,
                description: Some("CI configuration validated successfully".to_string()),
                context: "EkaCI: Configure".to_string(),
            };

            match client
                .create_commit_status(&ci_info.owner, &ci_info.repo_name, &ci_info.commit, request)
                .await
            {
                Ok(_) => {
                    info!(
                        "Completed configure gate commit status for commit {}",
                        ci_info.commit
                    );
                },
                Err(e) => {
                    warn!(
                        "Failed to complete configure gate commit status for {}: {:?}",
                        ci_info.commit, e
                    );
                },
            }
        }

        Ok(())
    }

    /// Update the status of a build in Gitea
    async fn handle_update_build_status(
        &self,
        drv_id: &crate::db::model::DrvId,
        status: &crate::db::model::build_event::DrvBuildState,
    ) -> Result<()> {
        debug!("Updating Gitea build status for {:?}: {:?}", drv_id, status);

        // Find all check runs associated with this derivation
        let check_runs =
            crate::db::gitea::check_runs_for_drv_path(drv_id, &self.db_service.pool).await?;

        for check_run in check_runs {
            let Some(client) = self.get_client(&check_run.domain).await else {
                warn!("No Gitea client for domain {}", check_run.domain);
                continue;
            };

            // Update the check run via actions module
            match crate::gitea::actions::update_build_check_run(
                &client,
                &check_run.repo_owner,
                &check_run.repo_name,
                check_run.check_run_id,
                status,
            )
            .await
            {
                Ok(()) => {
                    // Update database tracking
                    let state = build_state_to_gitea_state(status);
                    if let Err(e) = crate::db::gitea::update_check_run_status(
                        check_run.check_run_id,
                        &state,
                        &self.db_service.pool,
                    )
                    .await
                    {
                        warn!("Failed to update check run status in database: {:?}", e);
                    }
                },
                Err(e) => {
                    warn!(
                        "Failed to update check run {} for {:?}: {:?}",
                        check_run.check_run_id, drv_id, e
                    );
                },
            }
        }

        Ok(())
    }

    /// Handle CreateCIEvalJob task
    async fn handle_create_ci_eval_job(
        &self,
        ci_info: &crate::gitea::types::GiteaCIInfo,
        job_title: &str,
    ) -> Result<()> {
        debug!(
            "Creating CI eval job for job '{}' on commit {} in {}/{}",
            job_title, ci_info.commit, ci_info.owner, ci_info.repo_name
        );

        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No Gitea client for domain {}", ci_info.domain);
            return Ok(());
        };

        match crate::gitea::actions::create_ci_eval_job(&client, ci_info, job_title).await {
            Ok(check_run_id) => {
                self.eval_checks.lock().await.insert(
                    (ci_info.commit.clone(), job_title.to_string()),
                    check_run_id,
                );
                info!(
                    "Created CI eval job check run {} for job '{}' on commit {}",
                    check_run_id, job_title, ci_info.commit
                );
            },
            Err(e) => {
                warn!(
                    "Failed to create CI eval job for job '{}': {:?}",
                    job_title, e
                );
            },
        }

        Ok(())
    }

    /// Handle CompleteCIEvalJob task
    async fn handle_complete_ci_eval_job(
        &self,
        ci_info: &crate::gitea::types::GiteaCIInfo,
        job_name: &str,
        conclusion: &crate::gitea::types::GiteaCheckConclusion,
    ) -> Result<()> {
        debug!(
            "Completing CI eval job for job '{}' on commit {} with conclusion {:?}",
            job_name, ci_info.commit, conclusion
        );

        // Remove from tracking map
        let check_run_id = self
            .eval_checks
            .lock()
            .await
            .remove(&(ci_info.commit.clone(), job_name.to_string()));

        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No Gitea client for domain {}", ci_info.domain);
            return Ok(());
        };

        if let Some(check_run_id) = check_run_id {
            let success = matches!(
                conclusion,
                crate::gitea::types::GiteaCheckConclusion::Success
            );
            match crate::gitea::actions::update_ci_eval_job(
                &client,
                ci_info,
                job_name,
                check_run_id,
                success,
            )
            .await
            {
                Ok(()) => {
                    info!(
                        "Successfully completed CI eval job check run {} for job '{}'",
                        check_run_id, job_name
                    );
                },
                Err(e) => {
                    warn!(
                        "Failed to complete CI eval job for job '{}': {:?}",
                        job_name, e
                    );
                },
            }
        }

        Ok(())
    }

    /// Handle FailCIEvalJob task
    async fn handle_fail_ci_eval_job(
        &self,
        ci_info: &crate::gitea::types::GiteaCIInfo,
        job_name: &str,
        errors: &[crate::nix::nix_eval_jobs::NixEvalError],
    ) -> Result<()> {
        debug!(
            "Creating failed CI eval job for job '{}' on commit {} with {} errors",
            job_name,
            ci_info.commit,
            errors.len()
        );

        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No Gitea client for domain {}", ci_info.domain);
            return Ok(());
        };

        match crate::gitea::actions::fail_ci_eval_job(&client, ci_info, job_name, errors).await {
            Ok(check_run_id) => {
                info!(
                    "Created failed CI eval job check run {} for job '{}' on commit {} ({} errors)",
                    check_run_id,
                    job_name,
                    ci_info.commit,
                    errors.len()
                );
            },
            Err(e) => {
                warn!(
                    "Failed to create failed CI eval job for job '{}': {:?}",
                    job_name, e
                );
            },
        }

        Ok(())
    }

    /// Handle CancelCheckRunsForCommit task
    async fn handle_cancel_check_runs_for_commit(
        &self,
        ci_info: &crate::gitea::types::GiteaCIInfo,
    ) -> Result<()> {
        debug!(
            "Canceling check runs for commit {} in {}/{}",
            ci_info.commit, ci_info.owner, ci_info.repo_name
        );

        // Find all active check runs for this commit
        let check_runs =
            crate::db::gitea::check_runs_for_commit(&ci_info.commit, &self.db_service.pool).await?;

        for check_run in check_runs {
            let Some(client) = self.get_client(&check_run.domain).await else {
                warn!("No Gitea client for domain {}", check_run.domain);
                continue;
            };

            match crate::gitea::actions::cancel_check_run(
                &client,
                &check_run.repo_owner,
                &check_run.repo_name,
                check_run.check_run_id,
            )
            .await
            {
                Ok(()) => {
                    // Update database tracking
                    if let Err(e) = crate::db::gitea::update_check_run_status(
                        check_run.check_run_id,
                        "cancelled",
                        &self.db_service.pool,
                    )
                    .await
                    {
                        warn!("Failed to update check run status in database: {:?}", e);
                    }
                },
                Err(e) => {
                    warn!(
                        "Failed to cancel check run {}: {:?}",
                        check_run.check_run_id, e
                    );
                },
            }
        }

        Ok(())
    }

    /// Handle UpdateBuildStatusWithSizeWarning task
    async fn handle_update_build_status_with_size_warning(
        &self,
        drv_id: &crate::db::model::DrvId,
        status: &crate::db::model::build_event::DrvBuildState,
        baseline_size: u64,
        current_size: u64,
        increase_percent: f64,
    ) -> Result<()> {
        debug!(
            "Updating Gitea build status with size warning for {:?}: {:?}",
            drv_id, status
        );

        // Find all check runs associated with this derivation
        let check_runs =
            crate::db::gitea::check_runs_for_drv_path(drv_id, &self.db_service.pool).await?;

        let warning_message = format!(
            "Build succeeded with size warning: {}% increase ({} → {})",
            increase_percent as u64,
            crate::nix::size::format_size(baseline_size),
            crate::nix::size::format_size(current_size)
        );

        for check_run in check_runs {
            let Some(client) = self.get_client(&check_run.domain).await else {
                warn!("No Gitea client for domain {}", check_run.domain);
                continue;
            };

            // Create an update request with the size warning in the output
            let request = crate::gitea::client::UpdateCheckRunRequest {
                status: Some(crate::gitea::client::CheckStatus::Completed),
                conclusion: Some(crate::gitea::client::CheckConclusion::Success),
                output: Some(crate::gitea::client::CheckOutput {
                    title: "Build Succeeded with Size Warning".to_string(),
                    summary: warning_message.clone(),
                    text: None,
                }),
            };

            match client
                .update_check_run(
                    &check_run.repo_owner,
                    &check_run.repo_name,
                    check_run.check_run_id,
                    request,
                )
                .await
            {
                Ok(()) => {
                    // Update database tracking
                    if let Err(e) = crate::db::gitea::update_check_run_status(
                        check_run.check_run_id,
                        "success",
                        &self.db_service.pool,
                    )
                    .await
                    {
                        warn!("Failed to update check run status in database: {:?}", e);
                    }
                },
                Err(e) => {
                    warn!(
                        "Failed to update check run {} with size warning: {:?}",
                        check_run.check_run_id, e
                    );
                },
            }
        }

        Ok(())
    }

    /// Handle CreateFailureCheckRun task
    async fn handle_create_failure_check_run(
        &self,
        drv_id: &crate::db::model::DrvId,
        jobset_id: i64,
        job_attr_name: &str,
        difference: &crate::github::JobDifference,
    ) -> Result<()> {
        debug!(
            "Creating failure check run for {:?} (jobset {}, job '{}')",
            drv_id, jobset_id, job_attr_name
        );

        // Get jobset information to find the commit and repository
        let jobset_info: Option<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT sha, job, owner, repo_name, domain FROM GiteaJobSets WHERE ROWID = ?",
        )
        .bind(jobset_id)
        .fetch_optional(&self.db_service.pool)
        .await?;

        let Some((sha, _job, owner, repo_name, domain)) = jobset_info else {
            warn!("No jobset found with ROWID {}", jobset_id);
            return Ok(());
        };

        let Some(client) = self.get_client(&domain).await else {
            warn!("No Gitea client for domain {}", domain);
            return Ok(());
        };

        // Build description based on the difference type
        let description = match difference {
            crate::github::JobDifference::New => {
                format!("Job '{}' is new in this build", job_attr_name)
            },
            crate::github::JobDifference::Changed => {
                format!("Job '{}' failed: derivation changed", job_attr_name)
            },
            crate::github::JobDifference::Removed => {
                format!("Job '{}' was removed from the build set", job_attr_name)
            },
        };

        let check_name = format!("eka-ci/build/{}", job_attr_name);

        match crate::gitea::actions::create_failure_check_run(
            &client,
            &owner,
            &repo_name,
            &sha,
            &check_name,
            &description,
        )
        .await
        {
            Ok(check_run_id) => {
                info!(
                    "Created failure check run {} for job '{}' on commit {}",
                    check_run_id, job_attr_name, sha
                );

                // Get drv ROWID for database insertion
                if let Ok(Some(drv_rowid)) =
                    sqlx::query_scalar::<_, i64>("SELECT ROWID FROM Drv WHERE drv_path = ?")
                        .bind(drv_id)
                        .fetch_optional(&self.db_service.pool)
                        .await
                {
                    // Store in database for tracking
                    if let Err(e) = crate::db::gitea::insert_check_run_info(
                        check_run_id,
                        &sha,
                        &check_name,
                        &domain,
                        &owner,
                        &repo_name,
                        drv_rowid,
                        "failure",
                        &self.db_service.pool,
                    )
                    .await
                    {
                        warn!("Failed to insert check run info in database: {:?}", e);
                    }
                }
            },
            Err(e) => {
                warn!(
                    "Failed to create failure check run for job '{}': {:?}",
                    job_attr_name, e
                );
            },
        }

        Ok(())
    }

    // ---- Auto-merge evaluator ----

    async fn handle_check_auto_merge(
        &self,
        domain: &str,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
    ) -> Result<()> {
        info!(
            "Checking auto-merge eligibility for PR #{} in {}/{} on {}",
            pr_number, owner, repo_name, domain
        );

        let Some(client) = self.get_client(domain).await else {
            warn!("No Gitea client for domain {}", domain);
            return Ok(());
        };

        // Defer until head-commit jobset has fully succeeded
        if !crate::db::gitea::pr_head_build_succeeded(
            domain,
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?
        {
            info!(
                "PR #{} head build not yet successful, deferring auto-merge",
                pr_number
            );
            return Ok(());
        };

        // Look up PR by (domain, owner, repo_name, pr_number)
        let Some(pr) = crate::db::gitea::get_pull_request_row(
            domain,
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?
        else {
            warn!(
                "PR #{} not found in {}/{} on {} while evaluating auto-merge",
                pr_number, owner, repo_name, domain
            );
            return Ok(());
        };

        let pending_cmt_merge = pr.pending_comment_merge();

        // SHA-drift check: comment-merges are pinned to a commit
        if let Some(cmr) = pending_cmt_merge.as_ref() {
            if cmr.sha != pr.head_sha {
                self.cancel_drifted_comment_merge(
                    domain,
                    owner,
                    repo_name,
                    pr_number,
                    cmr,
                    &pr.head_sha,
                )
                .await?;
                return Ok(());
            }
        }

        // At least one merge path must be active
        if !pr.auto_merge_enabled && pending_cmt_merge.is_none() {
            debug!(
                "PR #{} in {}/{} on {} has no active auto-merge or comment-merge request; skipping",
                pr_number, owner, repo_name, domain
            );
            return Ok(());
        }

        let changed_packages = crate::db::gitea::get_pr_changed_packages(
            domain,
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?;

        if changed_packages.is_empty() {
            info!(
                "PR #{} has no changed packages, skipping auto-merge",
                pr_number
            );
            return Ok(());
        }

        // Maintainer-approval gate. Skipped for comment-driven merges
        if pending_cmt_merge.is_none() {
            let (eligible, missing_approvals) =
                crate::gitea::actions::check_pr_maintainer_approvals(
                    &client,
                    owner,
                    repo_name,
                    pr_number,
                    &changed_packages,
                    &self.db_service.pool,
                )
                .await?;

            if !eligible {
                info!(
                    "PR #{} is not eligible for auto-merge. Missing approvals for packages: {:?}",
                    pr_number, missing_approvals
                );
                return Ok(());
            }
        }

        // Method: comment request → PR-stored preference → "squash"
        let merge_method = pending_cmt_merge
            .as_ref()
            .and_then(|cmr| cmr.method.as_deref())
            .or(pr.merge_method.as_deref())
            .unwrap_or("squash");

        // Validate against repository settings before trying
        match crate::gitea::actions::validate_merge_method(&client, owner, repo_name, merge_method)
            .await
        {
            Ok(crate::gitea::actions::MergeMethodCheck::Ok) => {},
            Ok(crate::gitea::actions::MergeMethodCheck::NotAllowed { allowed }) => {
                warn!(
                    "PR #{} in {}/{} on {}: configured merge method '{}' is not allowed by \
                     repository settings (allowed: {:?}); skipping auto-merge",
                    pr_number, owner, repo_name, domain, merge_method, allowed
                );
                return Ok(());
            },
            Err(e) => {
                warn!(
                    "PR #{} in {}/{} on {}: failed to fetch repository merge settings: {:?}; \
                     skipping auto-merge",
                    pr_number, owner, repo_name, domain, e
                );
                return Ok(());
            },
        }

        self.auto_merge_execute(
            &client,
            domain,
            owner,
            repo_name,
            pr_number,
            merge_method,
            pending_cmt_merge.as_ref(),
        )
        .await;

        Ok(())
    }

    /// Notify requester and clear the pending row when a comment-merge's
    /// pinned SHA no longer matches the PR head.
    async fn cancel_drifted_comment_merge(
        &self,
        domain: &str,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        cmr: &crate::db::gitea::CommentMergeRequest,
        current_head: &str,
    ) -> Result<()> {
        warn!(
            "PR #{} in {}/{} on {}: comment-merge SHA drift (requested {}, now {}); cancelling",
            pr_number, owner, repo_name, domain, cmr.sha, current_head
        );

        // Best-effort notifications
        if let Err(e) = self
            .gitea_sender
            .send(GiteaTask::CommentMergeDriftCancelled {
                domain: domain.to_string(),
                owner: owner.to_string(),
                repo_name: repo_name.to_string(),
                pr_number,
                expected_sha: cmr.sha.clone(),
                actual_sha: current_head.to_string(),
                requester_login: cmr.requester_login.clone(),
            })
            .await
        {
            warn!("Failed to send CommentMergeDriftCancelled task: {:?}", e);
        }

        crate::db::gitea::clear_comment_merge(
            domain,
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?;
        Ok(())
    }

    /// Execute the merge + record post-conditions. Infallible at the
    /// caller level — a failed merge is logged and swallowed.
    async fn auto_merge_execute(
        &self,
        client: &GiteaClient,
        domain: &str,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        merge_method: &str,
        pending_cmt_merge: Option<&crate::db::gitea::CommentMergeRequest>,
    ) {
        info!(
            "Auto-merging PR #{} in {}/{} on {} using method '{}'",
            pr_number, owner, repo_name, domain, merge_method
        );

        let merge_request = crate::gitea::client::MergePullRequestRequest {
            merge_method: merge_method.to_string(),
            merge_message_field: None,
            merge_title_field: None,
        };

        match client
            .merge_pull_request(owner, repo_name, pr_number, merge_request)
            .await
        {
            Ok(_) => {
                info!(
                    "Successfully auto-merged PR #{} in {}/{} on {}",
                    pr_number, owner, repo_name, domain
                );

                // Mark as merged in database
                if let Err(e) = crate::db::gitea::mark_pr_merged(
                    domain,
                    owner,
                    repo_name,
                    pr_number,
                    pending_cmt_merge.map(|c| c.requester_id),
                    &self.db_service.pool,
                )
                .await
                {
                    warn!(
                        "Failed to mark PR #{} as merged in database: {:?}",
                        pr_number, e
                    );
                }
            },
            Err(e) => {
                warn!(
                    "Failed to auto-merge PR #{} in {}/{} on {}: {:?}",
                    pr_number, owner, repo_name, domain, e
                );
            },
        }
    }

    // ---- Comment-command handler ----

    #[allow(clippy::too_many_arguments)]
    async fn handle_process_merge_command(
        &self,
        domain: &str,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        comment_id: i64,
        requester_id: i64,
        requester_login: &str,
        body: &str,
        comment_created_at: &chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        use crate::gitea::webhook::comment_command::{CommentCommand, parse_comment_command};

        let Some(client) = self.get_client(domain).await else {
            warn!("No Gitea client for domain {}", domain);
            return Ok(());
        };

        // Re-parse rather than carrying a typed command
        let Some(cmd) = parse_comment_command(body) else {
            debug!(
                "Comment {} on PR #{} in {}/{} no longer parses as a command; dropping",
                comment_id, pr_number, owner, repo_name
            );
            return Ok(());
        };

        match cmd {
            CommentCommand::MergeCancel => {
                self.handle_merge_cancel(
                    &client,
                    domain,
                    owner,
                    repo_name,
                    pr_number,
                    requester_id,
                    requester_login,
                )
                .await
            },
            CommentCommand::Merge { method } => {
                self.handle_merge_accept(
                    &client,
                    domain,
                    owner,
                    repo_name,
                    pr_number,
                    comment_id,
                    requester_id,
                    requester_login,
                    method.as_ref().map(|m| m.as_str()),
                    comment_created_at,
                )
                .await
            },
        }
    }

    /// Outcome of an authorization check against a commenter.
    async fn authorize_commenter(
        &self,
        client: &GiteaClient,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        requester_id: i64,
        requester_login: &str,
    ) -> Result<Authorization> {
        let perm = match crate::gitea::actions::check_repo_permission_for_user(
            client,
            owner,
            repo_name,
            requester_login,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    "Failed to check repo permission for {} on {}/{}: {:?}",
                    requester_login, owner, repo_name, e
                );
                return Ok(Authorization::Abort);
            },
        };
        let has_write = perm.can_push || perm.is_admin;

        if has_write {
            return Ok(Authorization::Granted { has_write: true });
        }

        let changed = crate::db::gitea::get_pr_changed_packages(
            client.get_domain(),
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await
        .unwrap_or_default();

        let is_pkg_maintainer = if changed.is_empty() {
            false
        } else {
            crate::db::maintainers::is_maintainer_of_all_packages(
                requester_id,
                &changed,
                &self.db_service.pool,
            )
            .await
            .unwrap_or(false)
        };

        if is_pkg_maintainer {
            Ok(Authorization::Granted { has_write: false })
        } else {
            Ok(Authorization::Denied)
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_merge_cancel(
        &self,
        client: &GiteaClient,
        domain: &str,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        requester_id: i64,
        requester_login: &str,
    ) -> Result<()> {
        // Silent no-op when nothing is pending
        let pr_row = crate::db::gitea::get_pull_request_row(
            domain,
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?;

        let Some(pending) = pr_row.as_ref().and_then(|p| p.pending_comment_merge()) else {
            debug!(
                "No pending comment-merge on PR #{} in {}/{}; ignoring cancel from {}",
                pr_number, owner, repo_name, requester_login
            );
            return Ok(());
        };

        let is_self = requester_id == pending.requester_id;

        // Self-cancel fast path: skip API permission lookup
        let authorized = if is_self {
            true
        } else {
            match self
                .authorize_commenter(
                    client,
                    owner,
                    repo_name,
                    pr_number,
                    requester_id,
                    requester_login,
                )
                .await?
            {
                Authorization::Granted { .. } => true,
                Authorization::Denied => false,
                Authorization::Abort => return Ok(()),
            }
        };

        if !authorized {
            info!(
                "Denying @eka-ci merge cancel from {} on PR #{}: not the original requester, no \
                 repo write, and not a maintainer of all changed packages",
                requester_login, pr_number
            );
            if let Err(e) = client
                .create_issue_comment(
                    owner,
                    repo_name,
                    pr_number,
                    &format!(
                        "@{} I can't cancel this merge request — you must be the original \
                         requester, have write access to the repository, or be a maintainer of \
                         all affected packages.",
                        requester_login
                    ),
                )
                .await
            {
                warn!("Failed to post denial comment: {:?}", e);
            }
            return Ok(());
        }

        // Authorized: clear the pending request
        crate::db::gitea::clear_comment_merge(
            domain,
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_merge_accept(
        &self,
        client: &GiteaClient,
        domain: &str,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        comment_id: i64,
        requester_id: i64,
        requester_login: &str,
        method_str: Option<&str>,
        comment_created_at: &chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        match self
            .authorize_commenter(
                client,
                owner,
                repo_name,
                pr_number,
                requester_id,
                requester_login,
            )
            .await?
        {
            Authorization::Granted { .. } => {},
            Authorization::Abort => return Ok(()),
            Authorization::Denied => {
                info!(
                    "Denying @eka-ci merge from {} on PR #{}: no repo write and not a maintainer \
                     of all changed packages",
                    requester_login, pr_number
                );
                if let Err(e) = client
                    .create_issue_comment(
                        owner,
                        repo_name,
                        pr_number,
                        &format!(
                            "@{} I can't merge this PR — you need write access to the repository \
                             or be a maintainer of all affected packages.",
                            requester_login
                        ),
                    )
                    .await
                {
                    warn!("Failed to post denial comment: {:?}", e);
                }
                return Ok(());
            },
        }

        // Pin the merge to the current head SHA
        let Some(pr) = crate::db::gitea::get_pull_request_row(
            domain,
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?
        else {
            warn!(
                "PR #{} in {}/{} not found when processing merge command",
                pr_number, owner, repo_name
            );
            return Ok(());
        };

        if !self
            .check_push_timing(
                client,
                owner,
                repo_name,
                pr_number,
                requester_login,
                &pr.head_sha,
                comment_created_at,
            )
            .await?
        {
            return Ok(());
        }

        let rows = crate::db::gitea::set_comment_merge(
            domain,
            owner,
            repo_name,
            pr_number,
            &pr.head_sha,
            method_str,
            requester_id,
            requester_login,
            comment_id,
            &self.db_service.pool,
        )
        .await?;

        if rows == 0 {
            warn!(
                "set_comment_merge affected 0 rows for PR #{} in {}/{}",
                pr_number, owner, repo_name
            );
            return Ok(());
        }

        // Fire the evaluator in case gates are already green
        if let Err(e) = self
            .gitea_sender
            .send(GiteaTask::CheckAutoMerge {
                domain: domain.to_string(),
                owner: owner.to_string(),
                repo_name: repo_name.to_string(),
                pr_number,
            })
            .await
        {
            warn!("Failed to send CheckAutoMerge task: {:?}", e);
        }

        Ok(())
    }

    /// Best-effort force-push detection
    #[allow(clippy::too_many_arguments)]
    async fn check_push_timing(
        &self,
        client: &GiteaClient,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        requester_login: &str,
        head_sha: &str,
        comment_created_at: &chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        const PUSH_GRACE: chrono::Duration = chrono::Duration::seconds(30);

        match crate::gitea::actions::fetch_head_commit_date(client, owner, repo_name, head_sha)
            .await
        {
            Ok(Some(commit_date)) if commit_date > *comment_created_at + PUSH_GRACE => {
                info!(
                    "Refusing @eka-ci merge from {} on PR #{}: head commit {} committed at {} is \
                     newer than the command comment at {} (grace={}s); likely post-command push",
                    requester_login,
                    pr_number,
                    head_sha,
                    commit_date,
                    comment_created_at,
                    PUSH_GRACE.num_seconds()
                );
                if let Err(e) = client
                    .create_issue_comment(
                        owner,
                        repo_name,
                        pr_number,
                        &format!(
                            "@{} I can't merge this PR — the head commit (`{}`) appears to have \
                             been pushed after your `@eka-ci merge` command. Please review the \
                             latest changes and re-issue the command if you still want to merge.",
                            requester_login,
                            short_sha(head_sha),
                        ),
                    )
                    .await
                {
                    warn!("Failed to post push drift comment: {:?}", e);
                }
                Ok(false)
            },
            Ok(Some(_)) | Ok(None) | Err(_) => Ok(true),
        }
    }

    async fn handle_comment_merge_drift_cancelled(
        &self,
        domain: &str,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        expected_sha: &str,
        actual_sha: &str,
        requester_login: &str,
    ) -> Result<()> {
        let Some(client) = self.get_client(domain).await else {
            warn!("No Gitea client for domain {}", domain);
            return Ok(());
        };

        let body = format!(
            "@{} your `@eka-ci merge` request was cancelled because new commits landed on this PR \
             since you issued the command.\n\n- expected head: `{}`\n- current head: `{}`\n\nIf \
             you still want to merge, re-issue `@eka-ci merge` on the updated PR.",
            requester_login,
            short_sha(expected_sha),
            short_sha(actual_sha),
        );

        if let Err(e) = client
            .create_issue_comment(owner, repo_name, pr_number, &body)
            .await
        {
            warn!(
                "Failed to post SHA-drift comment on PR #{} in {}/{}: {:?}",
                pr_number, owner, repo_name, e
            );
        }
        Ok(())
    }

    async fn handle_create_dependency_changes_gate(
        &self,
        ci_info: &GiteaCIInfo,
        jobset_id: i64,
        base_jobset_id: i64,
    ) -> Result<()> {
        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!("No Gitea client for domain {}", ci_info.domain);
            return Ok(());
        };

        debug!(
            "Creating dependency changes gate for commit {} (jobset: {}, base: {})",
            &ci_info.commit, jobset_id, base_jobset_id
        );

        let comparisons = crate::dependency_comparison::compare_runtime_references_for_jobset(
            base_jobset_id,
            jobset_id,
            &self.db_service.pool,
        )
        .await?;

        let dependency_diff =
            crate::dependency_comparison::format_dependency_changes_as_diff(&comparisons);

        crate::gitea::actions::create_dependency_changes_gate(
            &client,
            ci_info,
            &dependency_diff,
            comparisons.len(),
        )
        .await?;

        debug!(
            "Successfully created dependency changes gate with {} packages affected",
            comparisons.len()
        );
        Ok(())
    }
}

/// Authorization outcome
enum Authorization {
    Granted {
        #[allow(dead_code)]
        has_write: bool,
    },
    Denied,
    Abort,
}

/// Short SHA for display
fn short_sha(sha: &str) -> &str {
    if sha.len() > 7 { &sha[..7] } else { sha }
}

/// Convert DrvBuildState to Gitea state string for database storage
fn build_state_to_gitea_state(state: &crate::db::model::build_event::DrvBuildState) -> String {
    use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult};

    match state {
        crate::db::model::build_event::DrvBuildState::Queued
        | crate::db::model::build_event::DrvBuildState::Buildable
        | crate::db::model::build_event::DrvBuildState::Blocked
        | crate::db::model::build_event::DrvBuildState::FailedRetry => "pending".to_string(),
        crate::db::model::build_event::DrvBuildState::Building => "running".to_string(),
        crate::db::model::build_event::DrvBuildState::Completed(DrvBuildResult::Success) => {
            "success".to_string()
        },
        crate::db::model::build_event::DrvBuildState::Completed(DrvBuildResult::Failure) => {
            "failure".to_string()
        },
        crate::db::model::build_event::DrvBuildState::TransitiveFailure => "failure".to_string(),
        crate::db::model::build_event::DrvBuildState::Interrupted(
            DrvBuildInterruptionKind::Cancelled,
        ) => "cancelled".to_string(),
        crate::db::model::build_event::DrvBuildState::Interrupted(_) => "failure".to_string(),
        crate::db::model::build_event::DrvBuildState::UnsatisfiableRequirements => {
            "failure".to_string()
        },
    }
}

/// Convert DrvBuildState to status and conclusion strings for database storage
#[allow(dead_code)]
fn status_to_strings(
    state: &crate::db::model::build_event::DrvBuildState,
) -> (String, Option<String>) {
    use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult};

    match state {
        crate::db::model::build_event::DrvBuildState::Queued
        | crate::db::model::build_event::DrvBuildState::Buildable
        | crate::db::model::build_event::DrvBuildState::Blocked
        | crate::db::model::build_event::DrvBuildState::FailedRetry => ("queued".to_string(), None),
        crate::db::model::build_event::DrvBuildState::Building => ("in_progress".to_string(), None),
        crate::db::model::build_event::DrvBuildState::Completed(DrvBuildResult::Success) => {
            ("completed".to_string(), Some("success".to_string()))
        },
        crate::db::model::build_event::DrvBuildState::Completed(DrvBuildResult::Failure) => {
            ("completed".to_string(), Some("failure".to_string()))
        },
        crate::db::model::build_event::DrvBuildState::TransitiveFailure => {
            ("completed".to_string(), Some("failure".to_string()))
        },
        crate::db::model::build_event::DrvBuildState::Interrupted(
            DrvBuildInterruptionKind::Cancelled,
        ) => ("completed".to_string(), Some("cancelled".to_string())),
        crate::db::model::build_event::DrvBuildState::Interrupted(_) => {
            ("completed".to_string(), Some("failure".to_string()))
        },
        crate::db::model::build_event::DrvBuildState::UnsatisfiableRequirements => {
            ("completed".to_string(), Some("failure".to_string()))
        },
    }
}

// ============================================================================
// AsyncService trait implementation
// ============================================================================

impl AsyncService<GiteaTask> for GiteaService {
    fn get_sender(&self) -> mpsc::Sender<GiteaTask> {
        self.gitea_sender.clone()
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<GiteaTask>> {
        self.gitea_receiver.take()
    }

    async fn handle_task(&self, task: GiteaTask) -> Result<()> {
        self.handle_gitea_task(&task).await
    }

    async fn handle_failure(&mut self, error: anyhow::Error) {
        error!("GiteaService task failed: {:?}", error);
    }

    async fn handle_closure(&mut self) {
        info!("GiteaService shutting down");
    }
}
