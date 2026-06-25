// Jobset and build management

use std::sync::Arc;

use anyhow::Result;
use tracing::{debug, warn};

use super::GitHubService;
use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;
use crate::github::service::{CHANGE_SUMMARY_DEBOUNCE, CICheckInfo, GitHubTask, actions};
use crate::nix::NixEvalDrv;

impl GitHubService {
    pub(super) async fn create_job_set(
        &self,
        ci_check_info: &std::sync::Arc<CICheckInfo>,
        name: &str,
        jobs: &[NixEvalDrv],
        config_json: Option<&str>,
    ) -> Result<()> {
        // Pass base commit SHA for incremental eval optimization
        // This allows job differences to be computed during insertion
        let base_sha = ci_check_info.base_commit.as_deref();

        let jobset_id = self
            .db_service
            .create_github_jobset_with_jobs(
                &ci_check_info.commit,
                name,
                &ci_check_info.owner,
                &ci_check_info.repo_name,
                jobs,
                config_json,
                base_sha,
            )
            .await?;

        // This is only relevant on PRs, missing a base commit denotes that
        // this jobset creation is done for a base_commit
        if let Some(base_commit) = ci_check_info.base_commit.as_ref() {
            // Eagerly create GitHub check runs for new/changed drvs
            self.github_sender
                .send(GitHubTask::CreateDrvCheckRuns {
                    ci_check_info: std::sync::Arc::clone(ci_check_info),
                    jobset_id,
                })
                .await?;

            // Queue dependency changes gate creation
            // This needs the base jobset ID to compare dependencies
            let base_jobset_id: Option<i64> =
                sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
                    .bind(base_commit)
                    .bind(name)
                    .fetch_optional(&self.db_service.pool)
                    .await?;

            if let Some(base_jobset_id) = base_jobset_id {
                self.github_sender
                    .send(GitHubTask::CreateDependencyChangesGate {
                        ci_check_info: std::sync::Arc::clone(ci_check_info),
                        jobset_id,
                        base_jobset_id,
                    })
                    .await?;
            }

            // Schedule the aggregated change-summary check, deduped per head SHA.
            if self
                .change_summary_pending
                .lock()
                .await
                .insert(ci_check_info.commit.clone())
            {
                self.spawn_change_summary_debounce(Arc::clone(ci_check_info), name.to_string());
            }
        }
        Ok(())
    }

    /// Spawn the debounce timer that enqueues a `CreateChangeSummaryCheck`.
    pub(super) fn spawn_change_summary_debounce(
        &self,
        ci_check_info: Arc<CICheckInfo>,
        job: String,
    ) {
        let sender = self.github_sender.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CHANGE_SUMMARY_DEBOUNCE).await;
            if let Err(e) = sender
                .send(GitHubTask::CreateChangeSummaryCheck { ci_check_info, job })
                .await
            {
                warn!(
                    "Failed to enqueue CreateChangeSummaryCheck after debounce: {:?}",
                    e
                );
            }
        });
    }

    pub(super) async fn handle_update_build_status_with_size_warning(
        &self,
        drv_id: &DrvId,
        status: &DrvBuildState,
        baseline_size: u64,
        current_size: u64,
        increase_percent: f64,
        threshold_percent: f64,
    ) -> Result<()> {
        let check_runs = self.db_service.check_runs_for_drv_path(drv_id).await?;
        for check_run in check_runs {
            debug!(
                "Updating checkrun with size warning for {}",
                &check_run.check_run_id
            );
            let octocrab = self.octocrab_for_owner(&check_run.repo_owner)?;
            actions::update_check_run_with_size_warning(
                &octocrab,
                &check_run,
                status,
                baseline_size,
                current_size,
                increase_percent,
                threshold_percent,
            )
            .await?;
        }
        Ok(())
    }
}
