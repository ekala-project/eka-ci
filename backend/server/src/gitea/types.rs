use std::sync::Arc;

use serde::Serialize;

use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;
use crate::github::JobDifference; // Reuse from GitHub
use crate::nix::nix_eval_jobs::{NixEvalDrv, NixEvalError};

/// Information needed to create a CI check for Gitea
/// Gitea uses a GitHub-compatible API, so this is similar to GitHub's CICheckInfo
/// but includes domain for self-hosted instances
#[derive(Debug, Clone)]
pub struct GiteaCIInfo {
    pub commit: String,
    pub base_commit: Option<String>,
    pub owner: String,
    pub repo_name: String,
    pub domain: String,
}

/// Task messages for GiteaService
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum GiteaTask {
    UpdateBuildStatus {
        drv_id: Arc<DrvId>,
        status: DrvBuildState,
    },
    UpdateBuildStatusWithSizeWarning {
        drv_id: Arc<DrvId>,
        status: DrvBuildState,
        baseline_size: u64,
        current_size: u64,
        increase_percent: f64,
        threshold_percent: f64,
    },
    CreateJobSet {
        ci_info: Arc<GiteaCIInfo>,
        name: String,
        jobs: Vec<NixEvalDrv>,
        config_json: Option<String>,
    },
    CreateCIConfigureGate {
        ci_info: Arc<GiteaCIInfo>,
    },
    CompleteCIConfigureGate {
        ci_info: Arc<GiteaCIInfo>,
    },
    CreateCIEvalJob {
        ci_info: Arc<GiteaCIInfo>,
        job_title: String,
    },
    CompleteCIEvalJob {
        ci_info: Arc<GiteaCIInfo>,
        job_name: String,
        conclusion: GiteaCheckConclusion,
    },
    CancelCheckRunsForCommit {
        ci_info: Arc<GiteaCIInfo>,
    },
    CreateFailureCheckRun {
        drv_id: Arc<DrvId>,
        jobset_id: i64,
        job_attr_name: String,
        difference: JobDifference,
    },
    FailCIEvalJob {
        ci_info: Arc<GiteaCIInfo>,
        job_name: String,
        errors: Vec<NixEvalError>,
    },
    CheckAutoMerge {
        domain: String,
        owner: String,
        repo_name: String,
        pr_number: i64,
    },
    CreateDependencyChangesGate {
        ci_info: Arc<GiteaCIInfo>,
        jobset_id: i64,
        base_jobset_id: i64,
    },
    /// Post (or update) the aggregated change-summary check for a PR head
    /// Newer Gitea versions support check runs; older versions fall back to commit statuses
    CreateChangeSummaryCheck {
        ci_info: Arc<GiteaCIInfo>,
        job: String,
    },
    /// Handle merge command from PR comment
    ProcessMergeCommand {
        domain: String,
        owner: String,
        repo_name: String,
        pr_number: i64,
        comment_id: i64,
        requester_id: i64,
        requester_login: String,
        body: String,
        comment_created_at: chrono::DateTime<chrono::Utc>,
    },
    /// Notify requester that comment-merge was cancelled due to SHA drift
    CommentMergeDriftCancelled {
        domain: String,
        owner: String,
        repo_name: String,
        pr_number: i64,
        expected_sha: String,
        actual_sha: String,
        requester_login: String,
    },
}

/// Gitea check run conclusions (GitHub-compatible)
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GiteaCheckConclusion {
    Success,
    Failure,
    Neutral,
    Cancelled,
    #[allow(dead_code)]
    TimedOut,
    #[allow(dead_code)]
    ActionRequired,
}

/// Gitea check run status (GitHub-compatible)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GiteaCheckStatus {
    Queued,
    InProgress,
    Completed,
}

/// Gitea commit status states (fallback for older instances)
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GiteaStatusState {
    Pending,
    Success,
    Error,
    Failure,
}

impl From<DrvBuildState> for GiteaCheckStatus {
    fn from(state: DrvBuildState) -> Self {
        match state {
            DrvBuildState::Queued | DrvBuildState::Buildable | DrvBuildState::FailedRetry => {
                Self::Queued
            },
            DrvBuildState::Building => Self::InProgress,
            DrvBuildState::Completed(_) | DrvBuildState::Interrupted(_) => Self::Completed,
            DrvBuildState::TransitiveFailure
            | DrvBuildState::Blocked
            | DrvBuildState::UnsatisfiableRequirements => Self::Completed,
        }
    }
}

impl From<DrvBuildState> for GiteaCheckConclusion {
    fn from(state: DrvBuildState) -> Self {
        use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult};

        match state {
            DrvBuildState::Completed(DrvBuildResult::Success) => Self::Success,
            DrvBuildState::Completed(DrvBuildResult::Failure) => Self::Failure,
            DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => Self::Cancelled,
            DrvBuildState::Interrupted(_) => Self::Failure,
            DrvBuildState::TransitiveFailure
            | DrvBuildState::Blocked
            | DrvBuildState::UnsatisfiableRequirements => Self::Failure,
            // Pending states shouldn't convert to conclusion
            _ => Self::Neutral,
        }
    }
}

impl From<DrvBuildState> for GiteaStatusState {
    fn from(state: DrvBuildState) -> Self {
        use crate::db::model::build_event::DrvBuildResult;

        match state {
            DrvBuildState::Queued
            | DrvBuildState::Buildable
            | DrvBuildState::FailedRetry
            | DrvBuildState::Building => Self::Pending,
            DrvBuildState::Completed(DrvBuildResult::Success) => Self::Success,
            DrvBuildState::Completed(DrvBuildResult::Failure) => Self::Failure,
            DrvBuildState::Interrupted(_)
            | DrvBuildState::TransitiveFailure
            | DrvBuildState::Blocked
            | DrvBuildState::UnsatisfiableRequirements => Self::Error,
        }
    }
}
