// GiteaService - CI integration with Gitea instances

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{Mutex, mpsc};
use tracing::{error, info, warn};

use crate::db::DbService;
use crate::gitea::GiteaClient;
use crate::gitea::types::GiteaTask;
use crate::graph::GraphServiceHandle;
use crate::metrics::ChangeSummaryMetrics;
use crate::services::AsyncService;

mod auto_merge;
mod change_summary;
mod checks;
mod dependency_gate;
pub mod helpers;
pub mod jobsets;

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
    /// Dedups change-summary tasks: only one pending per commit SHA
    change_summary_pending: Mutex<HashSet<String>>,
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
            change_summary_pending: Mutex::new(HashSet::new()),
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
                checks::handle_update_build_status(
                    drv_id,
                    status,
                    &self.db_service.pool,
                    &self.gitea_clients,
                )
                .await
            },
            GiteaTask::UpdateBuildStatusWithSizeWarning {
                drv_id,
                status,
                baseline_size,
                current_size,
                increase_percent,
                threshold_percent: _,
            } => {
                checks::handle_update_build_status_with_size_warning(
                    drv_id,
                    status,
                    *baseline_size,
                    *current_size,
                    *increase_percent,
                    &self.db_service.pool,
                    &self.gitea_clients,
                )
                .await
            },
            GiteaTask::CreateJobSet {
                ci_info,
                name,
                jobs,
                config_json,
            } => {
                jobsets::create_job_set(
                    ci_info,
                    name,
                    jobs,
                    config_json.as_deref(),
                    &self.db_service.pool,
                    &self.gitea_sender,
                    &self.change_summary_pending,
                )
                .await
            },
            GiteaTask::CreateCIConfigureGate { ci_info } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No Gitea client for domain {}", ci_info.domain);
                    return Ok(());
                };

                match checks::handle_create_ci_configure_gate(ci_info, &client).await {
                    Ok(check_run_id) => {
                        if check_run_id > 0 {
                            self.configure_checks
                                .lock()
                                .await
                                .insert(ci_info.commit.clone(), check_run_id);
                        }
                        Ok(())
                    },
                    Err(e) => Err(e),
                }
            },
            GiteaTask::CompleteCIConfigureGate { ci_info } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No Gitea client for domain {}", ci_info.domain);
                    return Ok(());
                };

                let check_run_id = self.configure_checks.lock().await.remove(&ci_info.commit);
                checks::handle_complete_ci_configure_gate(ci_info, check_run_id, &client).await
            },
            GiteaTask::CreateCIEvalJob { ci_info, job_title } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No Gitea client for domain {}", ci_info.domain);
                    return Ok(());
                };

                match checks::handle_create_ci_eval_job(ci_info, job_title, &client).await {
                    Ok(check_run_id) => {
                        self.eval_checks.lock().await.insert(
                            (ci_info.commit.clone(), job_title.to_string()),
                            check_run_id,
                        );
                        Ok(())
                    },
                    Err(e) => Err(e),
                }
            },
            GiteaTask::CompleteCIEvalJob {
                ci_info,
                job_name,
                conclusion,
            } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No Gitea client for domain {}", ci_info.domain);
                    return Ok(());
                };

                let check_run_id = self
                    .eval_checks
                    .lock()
                    .await
                    .remove(&(ci_info.commit.clone(), job_name.to_string()));

                if let Some(id) = check_run_id {
                    checks::handle_complete_ci_eval_job(ci_info, job_name, id, conclusion, &client)
                        .await
                } else {
                    Ok(())
                }
            },
            GiteaTask::FailCIEvalJob {
                ci_info,
                job_name,
                errors,
            } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No Gitea client for domain {}", ci_info.domain);
                    return Ok(());
                };

                checks::handle_fail_ci_eval_job(ci_info, job_name, errors, &client).await
            },
            GiteaTask::CancelCheckRunsForCommit { ci_info } => {
                checks::handle_cancel_check_runs_for_commit(
                    ci_info,
                    &self.db_service.pool,
                    &self.gitea_clients,
                )
                .await
            },
            GiteaTask::CreateFailureCheckRun {
                drv_id,
                jobset_id,
                job_attr_name,
                difference,
            } => {
                checks::handle_create_failure_check_run(
                    drv_id,
                    *jobset_id,
                    job_attr_name,
                    difference,
                    &self.db_service.pool,
                    &self.gitea_clients,
                )
                .await
            },
            GiteaTask::CreateChangeSummaryCheck { ci_info, job } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No Gitea client for domain {}", ci_info.domain);
                    return Ok(());
                };

                change_summary::handle_create_change_summary_check(
                    ci_info,
                    job,
                    &client,
                    &self.db_service.pool,
                    &self.graph_handle,
                    self.change_summary_metrics.as_ref(),
                )
                .await
            },
            GiteaTask::CheckAutoMerge {
                domain,
                owner,
                repo_name,
                pr_number,
            } => {
                let Some(client) = self.get_client(domain).await else {
                    warn!("No Gitea client for domain {}", domain);
                    return Ok(());
                };

                auto_merge::handle_check_auto_merge(
                    domain,
                    owner,
                    repo_name,
                    *pr_number,
                    &client,
                    &self.db_service,
                    &self.gitea_sender,
                )
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
                let Some(client) = self.get_client(domain).await else {
                    warn!("No Gitea client for domain {}", domain);
                    return Ok(());
                };

                auto_merge::commands::handle_process_merge_command(
                    domain,
                    owner,
                    repo_name,
                    *pr_number,
                    *comment_id,
                    *requester_id,
                    requester_login,
                    body,
                    comment_created_at,
                    &client,
                    &self.db_service,
                    &self.gitea_sender,
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
                let Some(client) = self.get_client(domain).await else {
                    warn!("No Gitea client for domain {}", domain);
                    return Ok(());
                };

                auto_merge::handle_comment_merge_drift_cancelled(
                    domain,
                    owner,
                    repo_name,
                    *pr_number,
                    expected_sha,
                    actual_sha,
                    requester_login,
                    &client,
                )
                .await
            },
            GiteaTask::CreateDependencyChangesGate {
                ci_info,
                jobset_id,
                base_jobset_id,
            } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No Gitea client for domain {}", ci_info.domain);
                    return Ok(());
                };

                dependency_gate::handle_create_dependency_changes_gate(
                    ci_info,
                    *jobset_id,
                    *base_jobset_id,
                    &client,
                    &self.db_service.pool,
                )
                .await
            },
        }
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
