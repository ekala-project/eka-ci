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

        crate::db::github::insert_check_run_info_with_node_id(
            check_run.id.0 as i64,
            drv_id,
            &jobset_info.repo_name,
            &jobset_info.owner,
            Some(&check_run.node_id),
            &self.db_service.pool,
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

    /// Re-push all check run states for a commit to GitHub.
    /// Uses GraphQL batching when node_ids are available, falls back to REST.
    pub(super) async fn handle_resync_check_runs(&self, sha: &str) -> Result<()> {
        let check_runs: Vec<crate::db::github::CheckRun> = sqlx::query_as(
            r#"
            SELECT DISTINCT c.check_run_id, c.repo_name, c.repo_owner, d.build_state, d.drv_path, c.node_id
            FROM GitHubCheckRuns c
            INNER JOIN Drv d ON c.drv_id = d.ROWID
            INNER JOIN Job j ON j.drv_id = d.ROWID
            INNER JOIN GitHubJobSets g ON j.jobset = g.ROWID
            WHERE g.sha = ?
            "#,
        )
        .bind(sha)
        .fetch_all(&self.db_service.pool)
        .await?;

        let total = check_runs.len();
        let mut batched = 0u32;
        let mut rest_updated = 0u32;
        let mut failed = 0u32;

        for check_run in &check_runs {
            let (gql_status, gql_conclusion) =
                crate::github::service::graphql_batch::build_state_to_graphql(
                    &check_run.build_state,
                );

            // Prefer GraphQL batcher when node_id is available
            if let Some(node_id) = &check_run.node_id {
                let repo_node_id = self
                    .get_repo_node_id(&check_run.repo_owner, &check_run.repo_name)
                    .await;
                if let Some(repo_node_id) = repo_node_id {
                    self.batcher
                        .queue_update(
                            &check_run.repo_owner,
                            &repo_node_id,
                            node_id,
                            gql_status,
                            gql_conclusion,
                        )
                        .await;
                    batched += 1;
                    continue;
                }
            }

            // Fallback: REST API
            self.rate_limiter.acquire().await;
            let octocrab = match self.octocrab_for_owner(&check_run.repo_owner) {
                Ok(o) => o,
                Err(e) => {
                    warn!(
                        "No installation for owner {} (check_run {}): {:?}",
                        check_run.repo_owner, check_run.check_run_id, e
                    );
                    failed += 1;
                    continue;
                },
            };
            match check_run
                .send_gh_update(&octocrab, &check_run.build_state)
                .await
            {
                Ok(_) => rest_updated += 1,
                Err(e) => {
                    failed += 1;
                    warn!(
                        "Failed to resync check_run {}: {:?}",
                        check_run.check_run_id, e
                    );
                },
            }
        }

        tracing::info!(
            "Resynced {} check runs for commit {} ({} batched, {} REST, {} failed)",
            total, sha, batched, rest_updated, failed
        );
        Ok(())
    }

    /// Look up the GraphQL node_id for a repository.
    pub(super) async fn get_repo_node_id(&self, owner: &str, repo: &str) -> Option<String> {
        // Check DB first
        let result: Option<Option<String>> = sqlx::query_scalar(
            "SELECT node_id FROM GitHubInstallationRepositories WHERE repo_owner = ? AND repo_name = ?",
        )
        .bind(owner)
        .bind(repo)
        .fetch_optional(&self.db_service.pool)
        .await
        .ok()?;

        if let Some(Some(node_id)) = result {
            if !node_id.is_empty() {
                return Some(node_id);
            }
        }

        // Fetch from API and cache
        let octocrab = self.octocrab_for_owner(owner).ok()?;
        let repo_info: serde_json::Value = octocrab
            .get(format!("/repos/{}/{}", owner, repo), None::<&()>)
            .await
            .ok()?;
        let node_id = repo_info.get("node_id")?.as_str()?.to_string();

        // Cache in DB
        let _ = sqlx::query(
            "UPDATE GitHubInstallationRepositories SET node_id = ? WHERE repo_owner = ? AND repo_name = ?",
        )
        .bind(&node_id)
        .bind(owner)
        .bind(repo)
        .execute(&self.db_service.pool)
        .await;

        Some(node_id)
    }

    /// Fetch the last N lines of a build log for a drv.
    /// Returns None if the log is unavailable.
    pub(super) async fn fetch_log_tail(&self, drv_id: &DrvId, lines: usize) -> Option<String> {
        let hash = drv_id.drv_hash();
        let dirs = shared::dirs::eka_dirs().ok()?;
        let log_path = dirs
            .get_data_home()
            .join("build-logs")
            .join(hash)
            .join("build.log");

        let content = tokio::fs::read_to_string(&log_path).await.ok()?;
        let all_lines: Vec<&str> = content.lines().collect();
        if all_lines.is_empty() {
            return None;
        }
        let start = all_lines.len().saturating_sub(lines);
        Some(all_lines[start..].join("\n"))
    }
}
