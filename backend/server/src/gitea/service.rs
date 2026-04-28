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
    gitea_receiver: Mutex<Option<mpsc::Receiver<GiteaTask>>>,
    /// Tracks configure gate check run IDs per commit
    configure_checks: Mutex<HashMap<String, i64>>,
    /// Tracks eval job check run IDs per (commit, job_name)
    eval_checks: Mutex<HashMap<(String, String), i64>>,
    /// Tracks change-summary check run IDs per commit
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
            gitea_receiver: Mutex::new(Some(gitea_receiver)),
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
        self.gitea_receiver.blocking_lock().take()
    }

    /// Get the Gitea client for a specific domain
    async fn get_client(&self, domain: &str) -> Option<Arc<GiteaClient>> {
        self.gitea_clients.lock().await.get(domain).cloned()
    }

    async fn handle_gitea_task(&self, task: &GiteaTask) -> Result<()> {
        match task {
            GiteaTask::UpdateBuildStatus { drv_id, status } => {
                debug!("Gitea UpdateBuildStatus for {:?}: {:?}", drv_id, status);
                // TODO: Query GiteaCheckRuns and update via Gitea API
                Ok(())
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
            GiteaTask::CreateChangeSummaryCheck { ci_info, job } => {
                self.handle_create_change_summary_check(ci_info, job).await
            },
            // Other tasks are stubs for now
            _ => {
                debug!("Gitea task not yet implemented: {:?}", task);
                Ok(())
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
        crate::db::github::create_jobs_for_jobset(jobset_id, jobs, &self.db_service.pool).await?;

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

        // TODO: Post markdown as check run annotation or PR comment via Gitea API
        // For now, just log that we would post it
        info!(
            "Would post change-summary check for PR in {}/{}/{} (domain: {})",
            ci_info.domain, ci_info.owner, ci_info.repo_name, ci_info.domain
        );
        debug!("Change summary markdown:\n{}", markdown);

        // Track the check run ID (would be returned by API)
        // self.change_summary_checks
        //     .lock()
        //     .await
        //     .insert(ci_info.commit.clone(), check_run_id);

        Ok(())
    }

    /// Create the initial CI configure gate check for a commit
    /// Uses check runs for newer Gitea, commit status for older
    async fn handle_create_ci_configure_gate(&self, ci_info: &Arc<GiteaCIInfo>) -> Result<()> {
        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!(
                "No Gitea client configured for domain {}, skipping configure gate",
                ci_info.domain
            );
            return Ok(());
        };

        if client.supports_check_runs() {
            // Use Check Runs API for newer Gitea
            let request = crate::gitea::client::CreateCheckRunRequest {
                name: "EkaCI: Configure".to_string(),
                head_sha: ci_info.commit.clone(),
                status: Some(crate::gitea::client::CheckStatus::InProgress),
                conclusion: None,
                output: Some(crate::gitea::client::CheckOutput {
                    title: "Configuring CI".to_string(),
                    summary: "Reading repository configuration...".to_string(),
                    text: None,
                }),
            };

            match client
                .create_check_run(&ci_info.owner, &ci_info.repo_name, request)
                .await
            {
                Ok(check_run) => {
                    info!(
                        "Created configure gate check run {} for commit {}",
                        check_run.id, ci_info.commit
                    );
                    self.configure_checks
                        .lock()
                        .await
                        .insert(ci_info.commit.clone(), check_run.id);
                },
                Err(e) => {
                    warn!(
                        "Failed to create configure gate check run for {}: {:?}",
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
        let Some(client) = self.get_client(&ci_info.domain).await else {
            warn!(
                "No Gitea client configured for domain {}, skipping configure gate completion",
                ci_info.domain
            );
            return Ok(());
        };

        if client.supports_check_runs() {
            // Update check run to success
            if let Some(check_run_id) = self.configure_checks.lock().await.remove(&ci_info.commit) {
                let request = crate::gitea::client::UpdateCheckRunRequest {
                    status: Some(crate::gitea::client::CheckStatus::Completed),
                    conclusion: Some(crate::gitea::client::CheckConclusion::Success),
                    output: Some(crate::gitea::client::CheckOutput {
                        title: "Configuration complete".to_string(),
                        summary: "CI configuration validated successfully".to_string(),
                        text: None,
                    }),
                };

                match client
                    .update_check_run(&ci_info.owner, &ci_info.repo_name, check_run_id, request)
                    .await
                {
                    Ok(_) => {
                        info!(
                            "Completed configure gate check run {} for commit {}",
                            check_run_id, ci_info.commit
                        );
                    },
                    Err(e) => {
                        warn!(
                            "Failed to complete configure gate check run for {}: {:?}",
                            ci_info.commit, e
                        );
                    },
                }
            }
        } else {
            // Update commit status to success
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
}

// ============================================================================
// AsyncService trait implementation
// ============================================================================

impl AsyncService<GiteaTask> for GiteaService {
    fn get_sender(&self) -> mpsc::Sender<GiteaTask> {
        self.gitea_sender.clone()
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<GiteaTask>> {
        self.gitea_receiver.blocking_lock().take()
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
