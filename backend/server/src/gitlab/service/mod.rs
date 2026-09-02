// GitLab CI service integration

mod auto_merge;
mod change_summary;
mod dependency_gate;
mod helpers;
mod jobsets;
mod statuses;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use helpers::short_sha;
use tokio::sync::{Mutex, mpsc};
use tracing::{error, info, warn};

use crate::db::DbService;
use crate::gitlab::GitLabClient;
use crate::gitlab::types::GitLabTask;
use crate::graph::GraphServiceHandle;
use crate::metrics::ChangeSummaryMetrics;
use crate::services::{AsyncService, TaskJournal};

/// GitLabService handles CI integration with GitLab instances
///
/// Unlike GitHub, GitLab uses commit statuses instead of check runs,
/// and posts detailed results as MR comments instead of check annotations.
pub struct GitLabService {
    db_service: DbService,
    gitlab_sender: mpsc::Sender<GitLabTask>,
    gitlab_receiver: Option<mpsc::Receiver<GitLabTask>>,
    /// Tracks configure gate status IDs per commit
    configure_statuses: Mutex<HashMap<String, i64>>,
    /// Tracks eval job status IDs per (commit, job_name)
    eval_statuses: Mutex<HashMap<(String, String), i64>>,
    /// Dedups change-summary tasks: only one pending per commit SHA
    change_summary_pending: Mutex<HashSet<String>>,
    /// Graph handle for rebuild impact analysis
    graph_handle: GraphServiceHandle,
    /// Optional metrics for observability
    change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
    /// GitLab API clients per domain (self-hosted instances)
    gitlab_clients: Mutex<HashMap<String, Arc<GitLabClient>>>,
    journal: TaskJournal<GitLabTask>,
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

        let pool = db_service.pool.clone();
        Ok(Self {
            db_service,
            gitlab_sender,
            gitlab_receiver: Some(gitlab_receiver),
            configure_statuses: Mutex::new(HashMap::new()),
            eval_statuses: Mutex::new(HashMap::new()),
            change_summary_pending: Mutex::new(HashSet::new()),
            graph_handle,
            change_summary_metrics,
            gitlab_clients: Mutex::new(gitlab_clients),
            journal: TaskJournal::new(pool, "gitlab"),
        })
    }

    pub fn get_sender(&self) -> mpsc::Sender<GitLabTask> {
        self.gitlab_sender.clone()
    }

    #[allow(dead_code)] // Called via AsyncService trait dispatch
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<GitLabTask>> {
        self.gitlab_receiver.take()
    }

    /// Get the GitLab client for a specific domain
    async fn get_client(&self, domain: &str) -> Option<Arc<GitLabClient>> {
        self.gitlab_clients.lock().await.get(domain).cloned()
    }

    async fn handle_gitlab_task(&self, task: &GitLabTask) -> Result<()> {
        match task {
            GitLabTask::UpdateBuildStatus { drv_id, status } => {
                let clients = self.gitlab_clients.lock().await;
                statuses::handle_update_build_status(
                    drv_id,
                    status,
                    &self.db_service.pool,
                    &clients,
                )
                .await
            },
            GitLabTask::UpdateBuildStatusWithSizeWarning {
                drv_id,
                status,
                baseline_size,
                current_size,
                increase_percent,
                threshold_percent: _,
            } => {
                let clients = self.gitlab_clients.lock().await;
                statuses::handle_update_build_status_with_size_warning(
                    drv_id,
                    status,
                    *baseline_size,
                    *current_size,
                    *increase_percent,
                    &self.db_service.pool,
                    &clients,
                )
                .await
            },
            GitLabTask::CreateJobSet {
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
                    &self.gitlab_sender,
                    &self.change_summary_pending,
                )
                .await
            },
            GitLabTask::CreateCIConfigureGate { ci_info } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No GitLab client for domain {}", ci_info.domain);
                    return Ok(());
                };
                statuses::handle_create_ci_configure_gate(
                    ci_info,
                    &client,
                    &self.configure_statuses,
                )
                .await
            },
            GitLabTask::CompleteCIConfigureGate { ci_info } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No GitLab client for domain {}", ci_info.domain);
                    return Ok(());
                };
                statuses::handle_complete_ci_configure_gate(
                    ci_info,
                    &client,
                    &self.configure_statuses,
                )
                .await
            },
            GitLabTask::CreateFailureStatus {
                drv_id,
                jobset_id,
                job_attr_name,
                difference,
            } => {
                let clients = self.gitlab_clients.lock().await;
                statuses::handle_create_failure_status(
                    drv_id,
                    *jobset_id,
                    job_attr_name,
                    difference,
                    &self.db_service.pool,
                    &clients,
                )
                .await
            },
            GitLabTask::CancelStatusesForCommit { ci_info } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No GitLab client for domain {}", ci_info.domain);
                    return Ok(());
                };
                statuses::handle_cancel_statuses_for_commit(ci_info, &client, &self.db_service.pool)
                    .await
            },
            GitLabTask::CreateCIEvalJob { ci_info, job_title } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No GitLab client for domain {}", ci_info.domain);
                    return Ok(());
                };
                statuses::handle_create_ci_eval_job(
                    ci_info,
                    job_title,
                    &client,
                    &self.eval_statuses,
                )
                .await
            },
            GitLabTask::CompleteCIEvalJob {
                ci_info,
                job_name,
                success,
            } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No GitLab client for domain {}", ci_info.domain);
                    return Ok(());
                };
                statuses::handle_complete_ci_eval_job(
                    ci_info,
                    job_name,
                    *success,
                    &client,
                    &self.eval_statuses,
                )
                .await
            },
            GitLabTask::FailCIEvalJob {
                ci_info,
                job_name,
                errors,
            } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No GitLab client for domain {}", ci_info.domain);
                    return Ok(());
                };
                statuses::handle_fail_ci_eval_job(ci_info, job_name, errors, &client).await
            },
            GitLabTask::CreateChangeSummaryComment { ci_info, job } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No GitLab client for domain {}", ci_info.domain);
                    return Ok(());
                };
                change_summary::handle_create_change_summary_comment(
                    ci_info,
                    job,
                    &client,
                    &self.db_service.pool,
                    &self.graph_handle,
                    self.change_summary_metrics.as_ref(),
                )
                .await
            },
            GitLabTask::CheckAutoMerge {
                domain,
                project_id,
                mr_iid,
            } => {
                let Some(client) = self.get_client(domain).await else {
                    warn!("No GitLab client for domain {}", domain);
                    return Ok(());
                };
                auto_merge::handle_check_auto_merge(
                    domain,
                    *project_id,
                    *mr_iid,
                    &client,
                    &self.db_service.pool,
                    &self.gitlab_sender,
                )
                .await
            },
            GitLabTask::ProcessMergeCommand {
                domain,
                project_id,
                mr_iid,
                note_id,
                requester_id,
                requester_username,
                body,
                note_created_at,
            } => {
                let Some(client) = self.get_client(domain).await else {
                    warn!("No GitLab client for domain {}", domain);
                    return Ok(());
                };
                auto_merge::commands::handle_process_merge_command(
                    domain,
                    *project_id,
                    *mr_iid,
                    *note_id,
                    *requester_id,
                    requester_username,
                    body,
                    note_created_at,
                    &client,
                    &self.db_service.pool,
                    &self.gitlab_sender,
                )
                .await
            },
            GitLabTask::CommentMergeDriftCancelled {
                domain,
                project_id,
                mr_iid,
                expected_sha,
                actual_sha,
                requester_username,
            } => {
                let Some(client) = self.get_client(domain).await else {
                    warn!("No GitLab client for domain {}", domain);
                    return Ok(());
                };

                let body = format!(
                    "@{} your `@eka-ci merge` request was cancelled because new commits landed on \
                     this MR since you issued the command.\n\n- expected head: `{}`\n- current \
                     head: `{}`\n\nIf you still want to merge, re-issue `@eka-ci merge` on the \
                     updated MR.",
                    requester_username,
                    short_sha(expected_sha),
                    short_sha(actual_sha),
                );

                if let Err(e) = client
                    .create_merge_request_note(*project_id, *mr_iid, &body)
                    .await
                {
                    warn!(
                        "Failed to post SHA-drift comment on MR !{} in project {}: {:?}",
                        mr_iid, project_id, e
                    );
                }
                Ok(())
            },
            GitLabTask::CreateDependencyChangesGate {
                ci_info,
                jobset_id,
                base_jobset_id,
            } => {
                let Some(client) = self.get_client(&ci_info.domain).await else {
                    warn!("No GitLab client for domain {}", ci_info.domain);
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

impl AsyncService<GitLabTask> for GitLabService {
    fn get_sender(&self) -> mpsc::Sender<GitLabTask> {
        self.gitlab_sender.clone()
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<GitLabTask>> {
        self.gitlab_receiver.take()
    }

    fn task_journal(&self) -> Option<&TaskJournal<GitLabTask>> {
        Some(&self.journal)
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
