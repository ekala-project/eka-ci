//! Size warnings on GitHub check runs.
//!
//! A successful build whose output grew beyond its job's `size_check`
//! threshold gets a *neutral* conclusion and a size comparison instead of the
//! plain success update. The recorder sends one task or the other, never
//! both, so the warning is not overwritten.

use anyhow::Result;
use octocrab::Octocrab;
use tracing::debug;

use super::GitHubService;
use crate::db::github::CheckRun;
use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;

/// The size comparison carried by `GitHubTask::UpdateBuildStatusWithSizeWarning`.
#[derive(Debug, Clone, Copy)]
pub(super) struct SizeWarningReport<'a> {
    /// Branch the baseline came from; empty if unknown.
    pub base_branch: &'a str,
    /// Baseline output size in bytes.
    pub baseline_size: u64,
    /// Output size of this build in bytes.
    pub current_size: u64,
    /// Increase over the baseline, in percent.
    pub increase_percent: f64,
    /// Configured `max_increase_percent`.
    pub threshold_percent: f64,
}

impl SizeWarningReport<'_> {
    /// Check-run title and markdown summary for this warning.
    pub(super) fn text(&self) -> (String, String) {
        use crate::nix::size::format_size;

        let diff_bytes = self.current_size as i64 - self.baseline_size as i64;
        let diff_formatted = if diff_bytes >= 0 {
            format!("+{}", format_size(diff_bytes as u64))
        } else {
            format!("-{}", format_size(diff_bytes.unsigned_abs()))
        };
        let branch = if self.base_branch.is_empty() {
            "base branch"
        } else {
            self.base_branch
        };

        let title = format!(
            "⚠️ Output size increased by {:.1}% (threshold: {:.1}%)",
            self.increase_percent, self.threshold_percent
        );
        let summary = format!(
            "The build output size has increased beyond the configured threshold.\n\n**Size \
             Comparison:**\n- **Baseline ({}):** {}\n- **Current build:** {}\n- **Difference:** \
             {} ({:+.1}%)\n- **Threshold:** {:.1}%\n\nThis check is set to **neutral** \
             (non-blocking) but indicates potential installation bloat. Consider reviewing the \
             changes to reduce output size.",
            branch,
            format_size(self.baseline_size),
            format_size(self.current_size),
            diff_formatted,
            self.increase_percent,
            self.threshold_percent
        );
        (title, summary)
    }
}

impl GitHubService {
    /// Mark every check run of `drv_id` neutral with the size warning.
    ///
    /// Coalesced gates cover several variants, so they get the normal
    /// aggregate update instead: a single variant's warning must not
    /// overwrite the state of the others.
    pub(super) async fn handle_update_build_status_with_size_warning(
        &self,
        drv_id: &DrvId,
        status: &DrvBuildState,
        warning: &SizeWarningReport<'_>,
    ) -> Result<()> {
        let check_runs = self.db_service.check_runs_for_drv_path(drv_id).await?;
        for check_run in check_runs {
            let variant_states = self
                .db_service
                .variant_states_for_check_run(check_run.check_run_id)
                .await?;
            if variant_states.len() > 1 {
                debug!(
                    "Check run {} is a coalesced gate; not showing the size warning",
                    check_run.check_run_id
                );
                self.update_coalesced_check_run(&check_run, drv_id, status, &variant_states)
                    .await?;
                continue;
            }

            debug!(
                "Updating check run {} with size warning: {:.1}% increase (threshold: {:.1}%)",
                check_run.check_run_id, warning.increase_percent, warning.threshold_percent
            );
            let (title, summary) = warning.text();
            if self.queue_size_warning(&check_run, &title, &summary).await {
                continue;
            }

            // REST fallback for check runs without a node id
            self.rate_limiter.acquire().await;
            let octocrab = self.octocrab_for_owner(&check_run.repo_owner)?;
            update_check_run_with_size_warning(&octocrab, &check_run, title, summary).await?;
        }
        Ok(())
    }

    /// Queue the warning on the GraphQL batcher, which also carries the
    /// check run's earlier status updates, so it lands after them. Returns
    /// `false` if the check run or repository has no node id.
    async fn queue_size_warning(&self, check_run: &CheckRun, title: &str, summary: &str) -> bool {
        let Some(node_id) = &check_run.node_id else {
            return false;
        };
        let Some(repo_node_id) = self
            .get_repo_node_id(&check_run.repo_owner, &check_run.repo_name)
            .await
        else {
            return false;
        };
        self.batcher
            .queue_update_with_output(
                &check_run.repo_owner,
                &repo_node_id,
                node_id,
                "COMPLETED",
                Some("NEUTRAL"),
                title.to_string(),
                summary.to_string(),
            )
            .await;
        true
    }
}

/// Update a check run over REST with a neutral conclusion and the size
/// warning as its output.
async fn update_check_run_with_size_warning(
    octocrab: &Octocrab,
    check_run: &CheckRun,
    title: String,
    summary: String,
) -> Result<()> {
    use octocrab::params::checks::{CheckRunConclusion, CheckRunOutput, CheckRunStatus};

    let output = CheckRunOutput {
        title,
        summary,
        text: None,
        annotations: vec![],
        images: vec![],
    };
    octocrab
        .checks(&check_run.repo_owner, &check_run.repo_name)
        .update_check_run(octocrab::models::CheckRunId(check_run.check_run_id as u64))
        .status(CheckRunStatus::Completed)
        .conclusion(CheckRunConclusion::Neutral)
        .output(output)
        .send()
        .await?;

    debug!(
        "Successfully updated check run {} with size warning",
        check_run.check_run_id
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_names_branch_and_sizes() {
        let report = SizeWarningReport {
            base_branch: "master",
            baseline_size: 1_000_000,
            current_size: 1_500_000,
            increase_percent: 50.0,
            threshold_percent: 10.0,
        };
        let (title, summary) = report.text();
        assert!(title.contains("50.0%"), "{title}");
        assert!(title.contains("threshold: 10.0%"), "{title}");
        assert!(summary.contains("**Baseline (master):**"), "{summary}");
        assert!(summary.contains("(+50.0%)"), "{summary}");
    }

    #[test]
    fn text_without_branch_falls_back() {
        let report = SizeWarningReport {
            base_branch: "",
            baseline_size: 2_000,
            current_size: 1_000,
            increase_percent: -50.0,
            threshold_percent: -60.0,
        };
        let (_, summary) = report.text();
        assert!(summary.contains("**Baseline (base branch):**"), "{summary}");
        assert!(summary.contains("**Difference:** -"), "{summary}");
    }
}
