//! Posting a [`BuildReport`] to the check runs of a derivation.

use anyhow::Result;
use octocrab::params::checks::CheckRunConclusion;
use tracing::debug;

use super::BuildReport;
use crate::db::github::CheckRun;
use crate::db::model::DrvId;
use crate::github::service::GitHubService;

impl GitHubService {
    /// Show `report` on every check run of `drv_id`.
    ///
    /// Coalesced gates cover several variants and keep their variant table,
    /// which the plain success update already refreshed.
    pub(in crate::github::service) async fn handle_report_build_success(
        &self,
        drv_id: &DrvId,
        report: &BuildReport,
    ) -> Result<()> {
        let check_runs = self.db_service.check_runs_for_drv_path(drv_id).await?;
        if check_runs.is_empty() {
            return Ok(());
        }
        let (title, summary) = report.render(&drv_id.store_path());
        let neutral = !report.warnings().is_empty();

        for check_run in check_runs {
            let variant_states = self
                .db_service
                .variant_states_for_check_run(check_run.check_run_id)
                .await?;
            if variant_states.len() > 1 {
                debug!(
                    "Check run {} is a coalesced gate; not showing the build report",
                    check_run.check_run_id
                );
                continue;
            }
            debug!(
                "Updating check run {} with the build report",
                check_run.check_run_id
            );
            self.update_completed_output(&check_run, neutral, &title, &summary)
                .await?;
        }
        Ok(())
    }

    /// Complete `check_run` as success (or neutral) with a custom output.
    ///
    /// Goes through the GraphQL batcher, which also carries the check run's
    /// earlier status updates, so it lands after them; falls back to REST
    /// for check runs or repositories without a node id.
    pub(in crate::github::service) async fn update_completed_output(
        &self,
        check_run: &CheckRun,
        neutral: bool,
        title: &str,
        summary: &str,
    ) -> Result<()> {
        if self
            .queue_completed_output(check_run, neutral, title, summary)
            .await
        {
            return Ok(());
        }
        self.rate_limiter.acquire().await;
        let octocrab = self.octocrab_for_owner(&check_run.repo_owner)?;
        let conclusion = if neutral {
            CheckRunConclusion::Neutral
        } else {
            CheckRunConclusion::Success
        };
        rest_completed_output(&octocrab, check_run, conclusion, title, summary).await
    }

    /// Returns `false` if the check run or repository has no node id.
    async fn queue_completed_output(
        &self,
        check_run: &CheckRun,
        neutral: bool,
        title: &str,
        summary: &str,
    ) -> bool {
        let Some(node_id) = &check_run.node_id else {
            return false;
        };
        let Some(repo_node_id) = self
            .get_repo_node_id(&check_run.repo_owner, &check_run.repo_name)
            .await
        else {
            return false;
        };
        let conclusion = if neutral { "NEUTRAL" } else { "SUCCESS" };
        self.batcher
            .queue_update_with_output(
                &check_run.repo_owner,
                &repo_node_id,
                node_id,
                "COMPLETED",
                Some(conclusion),
                title.to_string(),
                summary.to_string(),
            )
            .await;
        true
    }
}

/// Complete a check run over REST with `conclusion` and a custom output.
async fn rest_completed_output(
    octocrab: &octocrab::Octocrab,
    check_run: &CheckRun,
    conclusion: CheckRunConclusion,
    title: &str,
    summary: &str,
) -> Result<()> {
    use octocrab::params::checks::{CheckRunOutput, CheckRunStatus};

    let output = CheckRunOutput {
        title: title.to_string(),
        summary: crate::github::output_limit::cap_output_str(summary),
        text: None,
        annotations: vec![],
        images: vec![],
    };
    octocrab
        .checks(&check_run.repo_owner, &check_run.repo_name)
        .update_check_run(octocrab::models::CheckRunId(check_run.check_run_id as u64))
        .status(CheckRunStatus::Completed)
        .conclusion(conclusion)
        .output(output)
        .send()
        .await?;
    debug!(
        "Updated check run {} with a custom output (REST)",
        check_run.check_run_id
    );
    Ok(())
}
