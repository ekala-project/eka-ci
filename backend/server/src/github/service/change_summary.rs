// Change analysis and summary generation

use std::sync::Arc;

use anyhow::Result;
use tracing::{debug, warn};

use super::GitHubService;
use crate::dependency_comparison;
use crate::github::service::{CICheckInfo, actions};

impl GitHubService {
    pub(super) async fn handle_create_dependency_changes_gate(
        &self,
        ci_check_info: &CICheckInfo,
        jobset_id: i64,
        base_jobset_id: i64,
    ) -> Result<()> {
        let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

        debug!(
            "Creating dependency changes gate for commit {} (jobset: {}, base: {})",
            &ci_check_info.commit, jobset_id, base_jobset_id
        );

        let comparisons = dependency_comparison::compare_runtime_references_for_jobset(
            base_jobset_id,
            jobset_id,
            &self.db_service.pool,
        )
        .await?;

        let dependency_diff =
            dependency_comparison::format_dependency_changes_as_diff(&comparisons);

        actions::create_dependency_changes_gate(
            &octocrab,
            ci_check_info,
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

    /// Idempotently post (or patch) the aggregated change-summary check for a head SHA.
    pub(super) async fn handle_create_change_summary_check(
        &self,
        ci_check_info: &Arc<CICheckInfo>,
        job: &str,
    ) -> Result<()> {
        self.change_summary_pending
            .lock()
            .await
            .remove(&ci_check_info.commit);

        let Some(base_sha) = ci_check_info.base_commit.as_deref() else {
            debug!(
                "Skipping change-summary check for {}: no base commit (not a PR head)",
                &ci_check_info.commit
            );
            return Ok(());
        };

        let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

        // Resolve head jobset ID and metadata from GitHubJobSets
        let head_jobset: Option<(i64, String, String)> = sqlx::query_as(
            "SELECT ROWID, owner, repo_name FROM GitHubJobSets WHERE sha = ? AND job = ?",
        )
        .bind(&ci_check_info.commit)
        .bind(job)
        .fetch_optional(&self.db_service.pool)
        .await?;

        let Some((head_jobset_id, owner, repo_name)) = head_jobset else {
            debug!(
                "No head jobset for sha={} job={}; skipping change-summary check",
                &ci_check_info.commit, job
            );
            return Ok(());
        };

        // Resolve base jobset ID if it exists
        let base_jobset_id: Option<i64> =
            sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
                .bind(base_sha)
                .bind(job)
                .fetch_optional(&self.db_service.pool)
                .await?;

        // Create JobsetData for the head commit
        let jobset_data = crate::jobset_data::JobsetData::new(
            owner,
            repo_name,
            "github.com",
            &ci_check_info.commit,
            job,
            None, // config_json not needed for change summary
        );

        // Resolve options from jobset data
        let (opts, status) = crate::change_summary::resolve_options_from_jobset_data(
            &jobset_data,
            self.change_summary_metrics.as_deref(),
        )
        .await;

        // Build change summary using the new platform-agnostic function
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
                    &ci_check_info.commit, e
                );
                return Ok(());
            },
        };

        let markdown = summary.markdown;

        if let Some(existing) = self
            .change_summary_checks
            .lock()
            .await
            .get(&ci_check_info.commit)
            .copied()
        {
            if let Err(e) =
                actions::update_change_summary_check(&octocrab, ci_check_info, existing, markdown)
                    .await
            {
                warn!(
                    "Failed to update change-summary check {} for {}: {:?}",
                    existing, &ci_check_info.commit, e
                );
            }
        } else {
            match actions::create_change_summary_check(&octocrab, ci_check_info, markdown).await {
                Ok(check_run) => {
                    self.change_summary_checks
                        .lock()
                        .await
                        .insert(ci_check_info.commit.clone(), check_run.id);
                },
                Err(e) => {
                    warn!(
                        "Failed to create change-summary check for {}: {:?}",
                        &ci_check_info.commit, e
                    );
                },
            }
        }

        Ok(())
    }
}
