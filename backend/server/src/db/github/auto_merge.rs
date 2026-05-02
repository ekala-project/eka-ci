// GitHub Auto-Merge operations

use anyhow::Result;
use sqlx::{Pool, Sqlite};

use super::jobsets::{all_jobs_concluded, jobset_has_new_or_changed_failures};
use super::types::{CommentMergeRequest, PullRequest};

/// Enable auto-merge for a pull request
pub async fn enable_auto_merge(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    merge_method: Option<&str>,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        "UPDATE GitHubPullRequests
         SET auto_merge_enabled = TRUE, merge_method = ?
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(merge_method)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;
    Ok(())
}

/// Disable auto-merge for a pull request
pub async fn disable_auto_merge(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        "UPDATE GitHubPullRequests
         SET auto_merge_enabled = FALSE, merge_method = NULL
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a PR as merged; also clears any pending comment-merge.
pub async fn mark_pr_merged(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    merged_by_user_id: Option<i64>,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        "UPDATE GitHubPullRequests
         SET state = 'merged',
             merged_by_user_id = ?,
             merged_at = CURRENT_TIMESTAMP,
             comment_merge_sha = NULL,
             comment_merge_method = NULL,
             comment_merge_requester_id = NULL,
             comment_merge_requester_login = NULL,
             comment_merge_comment_id = NULL,
             comment_merge_requested_at = NULL
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(merged_by_user_id)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record a pending `@eka-ci merge` on a PR; overwrites any prior
/// request (single active request per PR). Returns rows updated
/// (0 means the PR row is missing — callers should treat as error).
pub async fn set_comment_merge_request(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    sha: &str,
    method: Option<&str>,
    requester_id: i64,
    requester_login: &str,
    comment_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<u64> {
    let rows = sqlx::query(
        "UPDATE GitHubPullRequests
         SET comment_merge_sha = ?,
             comment_merge_method = ?,
             comment_merge_requester_id = ?,
             comment_merge_requester_login = ?,
             comment_merge_comment_id = ?,
             comment_merge_requested_at = CURRENT_TIMESTAMP
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(sha)
    .bind(method)
    .bind(requester_id)
    .bind(requester_login)
    .bind(comment_id)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(rows)
}

/// Clear any pending comment-merge on a PR (on merge success, SHA
/// drift, explicit cancel, or PR close).
pub async fn clear_comment_merge_request(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        "UPDATE GitHubPullRequests
         SET comment_merge_sha = NULL,
             comment_merge_method = NULL,
             comment_merge_requester_id = NULL,
             comment_merge_requester_login = NULL,
             comment_merge_comment_id = NULL,
             comment_merge_requested_at = NULL
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch the pending comment-merge for a PR; `None` if none
/// outstanding or PR missing.
#[allow(dead_code)]
pub async fn get_comment_merge_request(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<CommentMergeRequest>> {
    let pr = sqlx::query_as::<_, PullRequest>(
        "SELECT * FROM GitHubPullRequests
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .fetch_optional(pool)
    .await?;
    Ok(pr.and_then(|pr| pr.pending_comment_merge()))
}

/// Get attribute paths (packages) changed in a PR by comparing base_sha to head_sha
pub async fn get_pr_changed_packages(
    pr_number: i64,
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<Vec<String>> {
    // Get the PR's head_sha jobset
    let jobset_id: Option<i64> = sqlx::query_scalar(
        "SELECT jobset_id FROM GitHubPullRequests pr
         JOIN GitHubJobSets gjs ON pr.head_sha = gjs.sha
         WHERE pr.pr_number = ? AND pr.owner = ? AND pr.repo_name = ?
         AND gjs.owner = ? AND gjs.repo_name = ?
         LIMIT 1",
    )
    .bind(pr_number)
    .bind(owner)
    .bind(repo_name)
    .bind(owner)
    .bind(repo_name)
    .fetch_optional(pool)
    .await?;

    let Some(jobset_id) = jobset_id else {
        // No jobset found, return empty list
        return Ok(vec![]);
    };

    // Get all unique attribute paths from job_difference for this jobset
    // Focus on changed and new packages (not removed)
    let attr_paths: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT attr_path FROM job_difference
         WHERE jobset_id = ? AND status IN ('changed', 'new')",
    )
    .bind(jobset_id)
    .fetch_all(pool)
    .await?;

    Ok(attr_paths)
}

/// Whether a PR's head commit has a fully built, non-failing jobset recorded.
///
/// Returns `true` only when:
/// - A jobset exists for the PR's head SHA in this owner/repo,
/// - every job in that jobset has reached a terminal state, and
/// - no new or changed jobs are in a failure state.
///
/// Intended as a guard before attempting approval-gated auto-merge, so that a
/// review submitted before builds complete does not cause premature merging.
pub async fn pr_head_build_succeeded(
    pr_number: i64,
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<bool> {
    let jobset_id: Option<i64> = sqlx::query_scalar(
        "SELECT gjs.ROWID FROM GitHubPullRequests pr
         JOIN GitHubJobSets gjs ON pr.head_sha = gjs.sha
         WHERE pr.pr_number = ? AND pr.owner = ? AND pr.repo_name = ?
         AND gjs.owner = ? AND gjs.repo_name = ?
         LIMIT 1",
    )
    .bind(pr_number)
    .bind(owner)
    .bind(repo_name)
    .bind(owner)
    .bind(repo_name)
    .fetch_optional(pool)
    .await?;

    let Some(jobset_id) = jobset_id else {
        return Ok(false);
    };

    if !all_jobs_concluded(jobset_id, pool).await? {
        return Ok(false);
    }

    if jobset_has_new_or_changed_failures(jobset_id, pool).await? {
        return Ok(false);
    }

    Ok(true)
}
