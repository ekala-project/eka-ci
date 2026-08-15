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
    /// Threshold for per-package check_run creation. When the number of
    /// new or changed packages is below this value, individual check_runs
    /// are created eagerly so each package is visible as a separate CI
    /// gate. At or above this threshold, individual check_runs are
    /// omitted (the eval gate serves as the summary) and only build
    /// failures produce per-package check_runs lazily.
    const EAGER_CHECK_RUN_THRESHOLD: usize = 500;

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
            // Create per-package check_runs and dispatch builds for
            // new/changed packages.
            self.create_eager_check_runs_and_dispatch_builds(ci_check_info, name, jobset_id)
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

    /// Create per-package check_runs for new/changed packages and
    /// dispatch build requests to the ingress service.
    ///
    /// Check_runs are created eagerly when the count is below
    /// `EAGER_CHECK_RUN_THRESHOLD`. For larger sets, the eval gate
    /// serves as the summary and only build failures produce
    /// per-package check_runs lazily.
    ///
    /// Build requests are always dispatched for all new/changed
    /// packages regardless of the check_run threshold.
    async fn create_eager_check_runs_and_dispatch_builds(
        &self,
        ci_check_info: &std::sync::Arc<CICheckInfo>,
        job_name: &str,
        jobset_id: i64,
    ) -> Result<()> {
        let changed_jobs = self.db_service.get_new_or_changed_jobs(jobset_id).await?;

        // Create per-package check_runs only below the threshold
        if !changed_jobs.is_empty() && changed_jobs.len() < Self::EAGER_CHECK_RUN_THRESHOLD {
            debug!(
                "Creating {} eager check_runs for new/changed packages in jobset {}",
                changed_jobs.len(),
                job_name,
            );

            let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

            for job in &changed_jobs {
                let check_run = ci_check_info
                    .create_gh_check_run(
                        &octocrab,
                        job_name,
                        &job.name,
                        job.build_state.clone(),
                        &job.difference,
                    )
                    .await?;

                self.db_service
                    .insert_check_run_info(
                        check_run.id.0 as i64,
                        &job.drv_path,
                        &ci_check_info.repo_name,
                        &ci_check_info.owner,
                    )
                    .await?;
            }
        } else if changed_jobs.len() >= Self::EAGER_CHECK_RUN_THRESHOLD {
            debug!(
                "Skipping eager check_run creation for {} new/changed packages (threshold {})",
                changed_jobs.len(),
                Self::EAGER_CHECK_RUN_THRESHOLD,
            );
        }

        // Dispatch build requests for all new/changed packages
        if let Some(ingress_sender) = &self.ingress_sender {
            debug!(
                "Dispatching {} ingress EvalRequests for new/changed packages",
                changed_jobs.len(),
            );
            let drv_ids: Vec<_> = changed_jobs
                .iter()
                .map(|j| std::sync::Arc::new(j.drv_path.clone()))
                .collect();

            for drv_id in &drv_ids {
                if let Err(e) = ingress_sender
                    .send(crate::scheduler::IngressTask::EvalRequest(
                        std::sync::Arc::clone(drv_id),
                    ))
                    .await
                {
                    warn!(
                        "Failed to send IngressTask for {}: {:?}",
                        drv_id.store_path(),
                        e
                    );
                    break;
                }
            }

            // Re-check buildability after a delay to catch packages
            // whose deps were marked Completed by the BFS cascade
            // running concurrently from other substitution checks.
            let sender = ingress_sender.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                for drv_id in drv_ids {
                    let _ = sender
                        .send(crate::scheduler::IngressTask::CheckBuildable(drv_id))
                        .await;
                }
            });
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
