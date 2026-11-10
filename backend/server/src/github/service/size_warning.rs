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
use crate::db::size::SizeMetric;

/// The size comparison carried by `GitHubTask::UpdateBuildStatusWithSizeWarning`.
#[derive(Debug, Clone, Copy)]
pub(super) struct SizeWarningReport<'a> {
    /// Which size grew.
    pub metric: SizeMetric,
    /// Branch the baseline came from; empty if unknown.
    pub base_branch: &'a str,
    /// Baseline size in bytes.
    pub baseline_size: u64,
    /// Size of this build in bytes.
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

        let (subject, advice) = match self.metric {
            SizeMetric::Output => (
                "output size",
                "installation bloat. Consider reviewing the changes to reduce output size.",
            ),
            SizeMetric::Closure => (
                "closure size (outputs plus runtime dependencies)",
                "dependency bloat. Consider reviewing the changes for new or heavier runtime \
                 dependencies.",
            ),
        };

        let title = format!(
            "⚠️ {} increased by {:.1}% (threshold: {:.1}%)",
            self.metric.label(),
            self.increase_percent,
            self.threshold_percent
        );
        let summary = format!(
            "The build {} has increased beyond the configured threshold.\n\n**Size \
             Comparison:**\n- **Baseline ({}):** {}\n- **Current build:** {}\n- **Difference:** \
             {} ({:+.1}%)\n- **Threshold:** {:.1}%\n\nThis check is set to **neutral** \
             (non-blocking) but indicates potential {}",
            subject,
            branch,
            format_size(self.baseline_size),
            format_size(self.current_size),
            diff_formatted,
            self.increase_percent,
            self.threshold_percent,
            advice
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
            metric: SizeMetric::Output,
            base_branch: "master",
            baseline_size: 1_000_000,
            current_size: 1_500_000,
            increase_percent: 50.0,
            threshold_percent: 10.0,
        };
        let (title, summary) = report.text();
        assert!(
            title.starts_with("⚠️ Output size increased by 50.0%"),
            "{title}"
        );
        assert!(title.contains("threshold: 10.0%"), "{title}");
        assert!(summary.contains("**Baseline (master):**"), "{summary}");
        assert!(summary.contains("(+50.0%)"), "{summary}");
    }

    /// The task is journaled: one written before `base_branch` and `metric`
    /// existed still deserializes, as an output-size warning.
    #[test]
    fn journaled_task_without_metric_is_output_size() {
        let json = r#"{"UpdateBuildStatusWithSizeWarning":{
            "drv_id":"jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv",
            "status":{"Completed":"Success"},
            "baseline_size":1,"current_size":2,
            "increase_percent":100.0,"threshold_percent":10.0}}"#;
        let task: crate::github::GitHubTask = serde_json::from_str(json).unwrap();
        match task {
            crate::github::GitHubTask::UpdateBuildStatusWithSizeWarning {
                metric,
                base_branch,
                ..
            } => assert_eq!((metric, base_branch.as_str()), (SizeMetric::Output, "")),
            other => panic!("unexpected task {:?}", other),
        }
    }

    #[test]
    fn text_without_branch_falls_back() {
        let report = SizeWarningReport {
            metric: SizeMetric::Closure,
            base_branch: "",
            baseline_size: 2_000,
            current_size: 1_000,
            increase_percent: -50.0,
            threshold_percent: -60.0,
        };
        let (title, summary) = report.text();
        assert!(title.starts_with("⚠️ Closure size"), "{title}");
        assert!(summary.contains("closure size (outputs plus"), "{summary}");
        assert!(summary.contains("**Baseline (base branch):**"), "{summary}");
        assert!(summary.contains("**Difference:** -"), "{summary}");
    }
}
