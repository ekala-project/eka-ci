// Shared types for GitHub database operations

use serde::Serialize;
use sqlx::FromRow;

use super::super::model::DrvId;
use super::super::model::build_event::DrvBuildState;
use crate::github::JobDifference;

#[derive(Clone, Debug, PartialEq, Eq, FromRow, Serialize)]
pub struct CheckRun {
    pub check_run_id: i64,
    pub repo_name: String,
    pub repo_owner: String,
    pub build_state: DrvBuildState,
    pub drv_path: DrvId,
    /// GraphQL node ID (e.g. "CR_kwDO..."). Used for batched GraphQL
    /// mutations. `None` for check runs created before this column existed.
    pub node_id: Option<String>,
}

/// Helper structure to represent a job from the base commit
#[derive(Debug, Clone)]
pub struct BaseJob {
    pub name: String,
    pub drv_path: String,
}

#[derive(Debug, FromRow)]
pub struct JobInfo {
    pub jobset_id: i64,
    pub name: String,
    pub difference: JobDifference,
}

/// A new or changed job in a jobset, used for eager check_run creation.
#[derive(Debug, FromRow)]
pub struct NewOrChangedJob {
    pub name: String,
    pub difference: JobDifference,
    pub drv_path: DrvId,
    pub build_state: DrvBuildState,
}

/// Get the jobset name and commit for a jobset ID
#[derive(Debug, FromRow)]
pub struct JobSetInfo {
    pub sha: String,
    pub job: String,
    pub owner: String,
    pub repo_name: String,
}

/// Repository information returned by the API
#[derive(Debug, FromRow, Serialize)]
pub struct RepositoryInfo {
    pub owner: String,
    pub repo_name: String,
    pub installation_id: i64,
}

/// Commit information with build status
#[derive(Debug, FromRow, Serialize)]
pub struct CommitInfo {
    pub sha: String,
    pub job_count: i64,
}

/// Job details for a specific jobset
#[derive(Debug, FromRow, Serialize)]
pub struct JobSetDetails {
    pub jobset_id: i64,
    pub job_name: String,
    pub sha: String,
    pub owner: String,
    pub repo_name: String,
    pub total_drvs: i64,
    pub queued_drvs: i64,
    pub buildable_drvs: i64,
    pub building_drvs: i64,
    pub completed_success_drvs: i64,
    pub completed_failure_drvs: i64,
    pub failed_retry_drvs: i64,
    pub transitive_failure_drvs: i64,
    pub blocked_drvs: i64,
    pub interrupted_drvs: i64,
}

/// Get all drvs for a specific jobset
#[derive(Debug, FromRow, Serialize)]
pub struct JobSetDrv {
    pub drv_path: DrvId,
    pub name: String,
    pub system: String,
    pub build_state: DrvBuildState,
    pub is_fod: bool,
    pub difference: JobDifference,
}

/// Summary of a jobset for repository listing, including change statistics
#[derive(Debug, FromRow, Serialize)]
pub struct RepositoryJobSetSummary {
    pub jobset_id: i64,
    pub job_name: String,
    pub sha: String,
    pub total_drvs: i64,
    pub queued_drvs: i64,
    pub buildable_drvs: i64,
    pub building_drvs: i64,
    pub failed_retry_drvs: i64,
    pub completed_success_drvs: i64,
    pub completed_failure_drvs: i64,
    pub transitive_failure_drvs: i64,
    pub blocked_drvs: i64,
    pub interrupted_drvs: i64,
    // Change summary counts
    pub new_jobs: i64,
    pub changed_jobs: i64,
    pub removed_jobs: i64,
}

/// A building derivation that may or may not be associated with a job
#[derive(Debug, FromRow, Serialize)]
pub struct BuildingDrv {
    pub drv_path: DrvId,
    pub name: Option<String>, // Name from Job table if associated
    pub system: String,
    pub build_state: DrvBuildState,
    pub is_fod: bool,
    pub difference: Option<JobDifference>, // Difference from Job table if associated
}

/// Get all jobs for a commit
#[derive(Debug, FromRow, Serialize)]
pub struct CommitJob {
    pub jobset_id: i64,
    pub job_name: String,
    pub total_drvs: i64,
    pub completed_drvs: i64,
    pub failed_drvs: i64,
}

/// Information about a pull request
#[derive(Clone, Debug, PartialEq, Eq, FromRow, Serialize)]
pub struct PullRequestInfo {
    pub pr_number: i64,
    pub owner: String,
    pub repo_name: String,
    pub head_sha: String,
    pub base_sha: String,
    pub title: String,
    pub author: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Pull request with associated job statistics
#[derive(Clone, Debug, Serialize)]
pub struct PullRequestWithStats {
    #[serde(flatten)]
    pub pr_info: PullRequestInfo,
    pub jobset_id: Option<i64>,
    pub total_drvs: i64,
    pub completed_success_drvs: i64,
    pub completed_failure_drvs: i64,
    pub failed_retry_drvs: i64,
    pub changed_drvs: i64,
    pub new_drvs: i64,
}

// Database row struct — all fields mirror the `GitHubPullRequests` schema so
// SQLx `FromRow` can hydrate them, even when Rust callers only read a subset.
#[allow(dead_code)]
#[derive(Debug, Clone, FromRow)]
pub struct PullRequest {
    pub pr_number: i64,
    pub owner: String,
    pub repo_name: String,
    pub head_sha: String,
    pub base_sha: String,
    pub title: String,
    pub author: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
    pub jobset_id: Option<i64>,
    pub is_merge_queue: bool,
    pub merge_group_head_sha: Option<String>,
    pub auto_merge_enabled: bool,
    pub merge_method: Option<String>,
    pub merged_by_user_id: Option<i64>,
    pub merged_at: Option<String>,
    // Pending `@eka-ci merge` request; all six NULL iff none pending.
    // See 20260419_pr_comment_merge.sql.
    pub comment_merge_sha: Option<String>,
    pub comment_merge_method: Option<String>,
    pub comment_merge_requester_id: Option<i64>,
    pub comment_merge_requester_login: Option<String>,
    pub comment_merge_comment_id: Option<i64>,
    pub comment_merge_requested_at: Option<String>,
}

/// Typed view of a pending `@eka-ci merge` on a PR (unresolved: not yet
/// merged, cancelled, or superseded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentMergeRequest {
    pub sha: String,
    pub method: Option<String>,
    pub requester_id: i64,
    pub requester_login: String,
    pub comment_id: i64,
    pub requested_at: String,
}

impl PullRequest {
    /// Pending comment-merge, if any. `comment_merge_*` columns are
    /// set together, so `comment_merge_sha` being `Some` implies the rest.
    pub fn pending_comment_merge(&self) -> Option<CommentMergeRequest> {
        Some(CommentMergeRequest {
            sha: self.comment_merge_sha.clone()?,
            method: self.comment_merge_method.clone(),
            requester_id: self.comment_merge_requester_id?,
            requester_login: self.comment_merge_requester_login.clone()?,
            comment_id: self.comment_merge_comment_id?,
            requested_at: self.comment_merge_requested_at.clone()?,
        })
    }
}
