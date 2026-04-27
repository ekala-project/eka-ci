use std::sync::Arc;

use serde::Serialize;

use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;
use crate::github::JobDifference; // Reuse from GitHub
use crate::nix::nix_eval_jobs::{NixEvalDrv, NixEvalError};

/// Information needed to create a CI commit status for GitLab
#[derive(Debug, Clone)]
pub struct GitLabCIInfo {
    pub commit: String,
    pub base_commit: Option<String>,
    pub owner: String,
    pub repo_name: String,
    pub project_id: i64,
    pub domain: String,
}

/// Task messages for GitLabService
#[derive(Debug, Clone)]
pub enum GitLabTask {
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
        ci_info: Arc<GitLabCIInfo>,
        name: String,
        jobs: Vec<NixEvalDrv>,
        config_json: Option<String>,
    },
    CreateCIConfigureGate {
        ci_info: Arc<GitLabCIInfo>,
    },
    CompleteCIConfigureGate {
        ci_info: Arc<GitLabCIInfo>,
    },
    CreateCIEvalJob {
        ci_info: Arc<GitLabCIInfo>,
        job_title: String,
    },
    CompleteCIEvalJob {
        ci_info: Arc<GitLabCIInfo>,
        job_name: String,
        success: bool,
    },
    CancelStatusesForCommit {
        ci_info: Arc<GitLabCIInfo>,
    },
    CreateFailureStatus {
        drv_id: Arc<DrvId>,
        jobset_id: i64,
        job_attr_name: String,
        difference: JobDifference,
    },
    FailCIEvalJob {
        ci_info: Arc<GitLabCIInfo>,
        job_name: String,
        errors: Vec<NixEvalError>,
    },
    CheckAutoMerge {
        domain: String,
        project_id: i64,
        mr_iid: i64,
    },
    CreateDependencyChangesGate {
        ci_info: Arc<GitLabCIInfo>,
        jobset_id: i64,
        base_jobset_id: i64,
    },
    /// Post (or update) the aggregated change-summary as an MR comment
    CreateChangeSummaryComment {
        ci_info: Arc<GitLabCIInfo>,
        job: String,
    },
    /// Handle merge command from MR note
    ProcessMergeCommand {
        domain: String,
        project_id: i64,
        mr_iid: i64,
        note_id: i64,
        requester_id: i64,
        requester_username: String,
        body: String,
        note_created_at: chrono::DateTime<chrono::Utc>,
    },
    /// Notify requester that comment-merge was cancelled due to SHA drift
    CommentMergeDriftCancelled {
        domain: String,
        project_id: i64,
        mr_iid: i64,
        expected_sha: String,
        actual_sha: String,
        requester_username: String,
    },
}

/// GitLab commit status states
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GitLabStatusState {
    Pending,
    Running,
    Success,
    Failed,
    Canceled,
}

impl From<DrvBuildState> for GitLabStatusState {
    fn from(state: DrvBuildState) -> Self {
        use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult};

        match state {
            DrvBuildState::Queued | DrvBuildState::Buildable => Self::Pending,
            DrvBuildState::FailedRetry => Self::Pending, // Will retry
            DrvBuildState::Building => Self::Running,
            DrvBuildState::Completed(DrvBuildResult::Success) => Self::Success,
            DrvBuildState::Completed(DrvBuildResult::Failure) => Self::Failed,
            DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => Self::Canceled,
            DrvBuildState::Interrupted(_) => Self::Failed,
            DrvBuildState::TransitiveFailure
            | DrvBuildState::Blocked
            | DrvBuildState::UnsatisfiableRequirements => Self::Failed,
        }
    }
}
