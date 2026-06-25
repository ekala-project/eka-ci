// Check run management

use anyhow::Result;
use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};
use tracing::{debug, warn};

use super::GitHubService;
use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;
use crate::github::service::{CICheckInfo, JobDifference, actions};

impl GitHubService {
    pub(super) async fn handle_cancel_check_runs_for_commit(
        &self,
        ci_check_info: &CICheckInfo,
    ) -> Result<()> {
        let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

        // Cancel any in-progress configure gate.
        if let Some(check_run_id) = self
            .github_configure_checks
            .lock()
            .await
            .remove(&ci_check_info.commit)
        {
            if let Err(e) = actions::update_ci_configure_gate(
                &octocrab,
                ci_check_info,
                check_run_id,
                CheckRunStatus::Completed,
                CheckRunConclusion::Cancelled,
            )
            .await
            {
                warn!(
                    "Failed to cancel configure gate for {}: {:?}",
                    &ci_check_info.commit, e
                );
            }
        }

        // Cancel in-progress eval gates (may be multiple per commit).
        let keys_to_remove: Vec<_> = self
            .github_eval_checks
            .lock()
            .await
            .keys()
            .filter(|(commit, _)| commit == &ci_check_info.commit)
            .cloned()
            .collect();

        for key in keys_to_remove {
            if let Some(check_run_id) = self.github_eval_checks.lock().await.remove(&key) {
                if let Err(e) = actions::update_ci_eval_job(
                    &octocrab,
                    ci_check_info,
                    check_run_id,
                    CheckRunStatus::Completed,
                    CheckRunConclusion::Cancelled,
                )
                .await
                {
                    warn!(
                        "Failed to cancel eval gate for {}: {:?}",
                        &ci_check_info.commit, e
                    );
                }
            }
        }

        // Cancel all job check_runs for this commit.
        let check_runs = self
            .db_service
            .check_runs_for_commit(&ci_check_info.commit)
            .await?;
        for check_run in check_runs {
            if let Err(e) = check_run
                .send_gh_update(
                    &octocrab,
                    &DrvBuildState::Interrupted(
                        crate::db::model::build_event::DrvBuildInterruptionKind::Cancelled,
                    ),
                )
                .await
            {
                warn!(
                    "Failed to cancel check_run {} for {}: {:?}",
                    check_run.check_run_id, &ci_check_info.commit, e
                );
            }
        }
        Ok(())
    }

    pub(super) async fn handle_create_failure_check_run(
        &self,
        drv_id: &DrvId,
        jobset_id: i64,
        job_attr_name: &str,
        difference: &JobDifference,
    ) -> Result<()> {
        let jobset_info = self.db_service.get_jobset_info(jobset_id).await?;
        let octocrab = self.octocrab_for_owner(&jobset_info.owner)?;

        let drv = self.db_service.get_drv(drv_id).await?;
        let state = drv
            .map(|x| x.build_state)
            .unwrap_or(DrvBuildState::Completed(
                crate::db::model::build_event::DrvBuildResult::Failure,
            ));

        let ci_check_info = CICheckInfo {
            commit: jobset_info.sha.clone(),
            base_commit: None,
            owner: jobset_info.owner.clone(),
            repo_name: jobset_info.repo_name.clone(),
        };

        let check_run = ci_check_info
            .create_gh_check_run(
                &octocrab,
                &jobset_info.job,
                job_attr_name,
                state,
                difference,
            )
            .await?;

        self.db_service
            .insert_check_run_info(
                check_run.id.0 as i64,
                drv_id,
                &jobset_info.repo_name,
                &jobset_info.owner,
            )
            .await?;
        Ok(())
    }

    pub(super) async fn handle_create_check_run(
        &self,
        owner: &str,
        repo_name: &str,
        sha: &str,
        check_name: &str,
        check_result_id: i64,
    ) -> Result<()> {
        let octocrab = self.octocrab_for_owner(owner)?;
        let check_run =
            actions::create_check_run(&octocrab, owner, repo_name, sha, check_name).await?;
        self.db_service
            .insert_check_run_info_for_check(
                check_run.id.0 as i64,
                check_result_id,
                repo_name,
                owner,
            )
            .await?;
        Ok(())
    }

    /// Eagerly create GitHub check runs for each new/changed drv in a jobset.
    pub(super) async fn handle_create_drv_check_runs(
        &self,
        ci_check_info: &std::sync::Arc<CICheckInfo>,
        jobset_id: i64,
    ) -> Result<()> {
        let jobs = self
            .db_service
            .get_new_and_changed_jobs_for_jobset(jobset_id)
            .await?;

        if jobs.is_empty() {
            debug!("No new/changed jobs in jobset {}, skipping check run creation", jobset_id);
            return Ok(());
        }

        let jobset_info = self.db_service.get_jobset_info(jobset_id).await?;
        let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

        debug!(
            "Creating {} check runs for new/changed drvs in jobset {} ({})",
            jobs.len(),
            jobset_id,
            &jobset_info.job
        );

        for (drv_id, attr_name, difference) in &jobs {
            let initial_state = DrvBuildState::Queued;
            match ci_check_info
                .create_gh_check_run(&octocrab, &jobset_info.job, attr_name, initial_state, difference)
                .await
            {
                Ok(check_run) => {
                    if let Err(e) = self
                        .db_service
                        .insert_check_run_info(
                            check_run.id.0 as i64,
                            drv_id,
                            &ci_check_info.repo_name,
                            &ci_check_info.owner,
                        )
                        .await
                    {
                        warn!(
                            "Failed to store check run info for {} ({:?}): {:?}",
                            attr_name, drv_id, e
                        );
                    }
                },
                Err(e) => {
                    warn!(
                        "Failed to create check run for {} ({:?}): {:?}",
                        attr_name, drv_id, e
                    );
                },
            }
        }

        Ok(())
    }

    /// Create or update a GitHub check run for a release channel promotion.
    ///
    /// The check run name is `release/{channel_name}` and displays the
    /// current promotion status (Evaluating → in_progress, Promoted →
    /// success, Blocked → failure, Skipped → neutral).
    pub(super) async fn handle_create_channel_promotion_check(
        &self,
        owner: &str,
        repo_name: &str,
        sha: &str,
        channel_name: &str,
        promotion_status: crate::channels::types::PromotionStatus,
        blocked_reason: Option<&str>,
    ) -> Result<()> {
        use crate::channels::types::PromotionStatus;

        let octocrab = self.octocrab_for_owner(owner)?;
        let check_name = format!("release/{}", channel_name);

        // Map promotion status to GitHub check run status + conclusion
        let (status, conclusion, summary) = match promotion_status {
            PromotionStatus::Evaluating => (
                "in_progress",
                None,
                format!(
                    "Channel `{}` is evaluating commit for promotion to target branch.",
                    channel_name
                ),
            ),
            PromotionStatus::Promoted => (
                "completed",
                Some("success"),
                format!(
                    "Channel `{}` successfully promoted this commit to the target branch.",
                    channel_name
                ),
            ),
            PromotionStatus::Blocked => {
                let reason = blocked_reason.unwrap_or("unknown reason");
                (
                    "completed",
                    Some("failure"),
                    format!("Channel `{}` promotion blocked: {}", channel_name, reason),
                )
            },
            PromotionStatus::Skipped => (
                "completed",
                Some("neutral"),
                format!(
                    "Channel `{}` skipped this commit (superseded by a newer SHA).",
                    channel_name
                ),
            ),
            PromotionStatus::PushFailed => (
                "completed",
                Some("failure"),
                format!(
                    "Channel `{}` promotion failed during git push (likely non-fast-forward).",
                    channel_name
                ),
            ),
        };

        // Use octocrab's check run builder. We create a new check run
        // each time rather than updating; GitHub deduplicates by
        // (name, sha) automatically.
        let route = format!("/repos/{}/{}/check-runs", owner, repo_name);

        #[derive(serde::Serialize)]
        struct CreateCheckRunRequest<'a> {
            name: &'a str,
            head_sha: &'a str,
            status: &'a str,
            #[serde(skip_serializing_if = "Option::is_none")]
            conclusion: Option<&'a str>,
            output: Output<'a>,
        }

        #[derive(serde::Serialize)]
        struct Output<'a> {
            title: &'a str,
            summary: &'a str,
        }

        let request = CreateCheckRunRequest {
            name: &check_name,
            head_sha: sha,
            status,
            conclusion,
            output: Output {
                title: &check_name,
                summary: &summary,
            },
        };

        octocrab._post(route, Some(&request)).await.map_err(|e| {
            anyhow::anyhow!(
                "failed to create channel promotion check run for {}: {:?}",
                check_name,
                e
            )
        })?;

        debug!(
            event = "channel_check_run_created",
            owner = %owner,
            repo = %repo_name,
            sha = %sha,
            channel = %channel_name,
            status = ?promotion_status,
            "created GitHub check run for channel promotion"
        );

        Ok(())
    }
}
