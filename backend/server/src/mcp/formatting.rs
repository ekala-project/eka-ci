use crate::db::github::PullRequestWithStats;
use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};

pub(super) fn format_build_state(state: &DrvBuildState) -> &'static str {
    match state {
        DrvBuildState::Queued => "queued",
        DrvBuildState::Buildable => "buildable",
        DrvBuildState::FailedRetry => "retrying",
        DrvBuildState::Building => "building",
        DrvBuildState::Completed(DrvBuildResult::Success) => "success",
        DrvBuildState::Completed(DrvBuildResult::Failure) => "failed",
        DrvBuildState::TransitiveFailure => "transitive failure",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::OutOfMemory) => "interrupted (OOM)",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout) => "interrupted (timeout)",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => {
            "interrupted (cancelled)"
        },
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath) => {
            "interrupted (process death)"
        },
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::SchedulerDeath) => {
            "interrupted (scheduler death)"
        },
        DrvBuildState::Blocked => "blocked",
        DrvBuildState::UnsatisfiableRequirements => "unsatisfiable requirements",
    }
}

pub(super) fn format_pr_summary(pr: &PullRequestWithStats) -> String {
    format!(
        "#{} {}/{}: {} [{} failed, {} success, {} total] (by {})",
        pr.pr_info.pr_number,
        pr.pr_info.owner,
        pr.pr_info.repo_name,
        pr.pr_info.title,
        pr.completed_failure_drvs,
        pr.completed_success_drvs,
        pr.total_drvs,
        pr.pr_info.author,
    )
}

pub(super) fn truncate_log(contents: &str, tail_lines: usize) -> String {
    let lines: Vec<&str> = contents.lines().collect();
    if lines.len() <= tail_lines {
        return contents.to_string();
    }
    let start = lines.len() - tail_lines;
    let truncated: String = lines[start..].join("\n");
    format!(
        "... ({} lines omitted, showing last {}) ...\n{}",
        start, tail_lines, truncated
    )
}
