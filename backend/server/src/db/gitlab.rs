///! Database helper functions for GitLab platform operations.
///!
///! This module provides GitLab-specific database operations for commit statuses,
///! merge requests, and related data. It follows the same patterns as the GitHub
///! helpers in `db/github.rs`.
use anyhow::Result;
use sqlx::{FromRow, Pool, Sqlite};

use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;

/// A GitLab commit status linked to a derivation.
///
/// This struct represents a row from the GitLabCommitStatuses table,
/// joining with the Drv table to get the current build state.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct CommitStatus {
    /// GitLab API status ID
    pub status_id: i64,
    /// Git commit SHA
    pub sha: String,
    /// Status name/context (e.g., "eka-ci/build")
    pub name: String,
    /// GitLab numeric project ID
    pub project_id: i64,
    /// Platform domain (e.g., "gitlab.com", "gitlab.example.com")
    pub domain: String,
    /// Repository owner/group
    pub repo_owner: String,
    /// Repository name
    pub repo_name: String,
    /// Current build state of the associated derivation
    pub build_state: DrvBuildState,
    /// Derivation path
    pub drv_path: DrvId,
    /// Status state: "pending", "running", "success", "failed", "canceled"
    pub state: String,
}

/// Return all commit statuses which match a drv_path.
///
/// This is used when updating build status - we find all commit statuses
/// associated with a derivation and update them.
pub async fn commit_statuses_for_drv_path(
    drv_path: &DrvId,
    pool: &Pool<Sqlite>,
) -> Result<Vec<CommitStatus>> {
    let statuses = sqlx::query_as(
        r#"
        SELECT
            cs.status_id, cs.sha, cs.name, cs.project_id, cs.domain,
            cs.repo_owner, cs.repo_name, d.build_state, d.drv_path, cs.state
        FROM GitLabCommitStatuses cs
        INNER JOIN Drv d ON cs.drv_id = d.ROWID
        WHERE d.drv_path = ?
        "#,
    )
    .bind(drv_path)
    .fetch_all(pool)
    .await?;

    Ok(statuses)
}

/// Return all commit statuses for a specific commit SHA that are still active
/// (pending or running state).
///
/// This is used when cancelling builds - we find all active statuses for a commit
/// and mark them as cancelled.
pub async fn commit_statuses_for_commit(
    sha: &str,
    pool: &Pool<Sqlite>,
) -> Result<Vec<CommitStatus>> {
    let statuses = sqlx::query_as(
        r#"
        SELECT DISTINCT
            cs.status_id, cs.sha, cs.name, cs.project_id, cs.domain,
            cs.repo_owner, cs.repo_name, d.build_state, d.drv_path, cs.state
        FROM GitLabCommitStatuses cs
        INNER JOIN Drv d ON cs.drv_id = d.ROWID
        INNER JOIN GitLabJob j ON j.drv_id = d.ROWID
        INNER JOIN GitLabJobSets g ON j.jobset = g.ROWID
        WHERE g.sha = ? AND cs.state IN ('pending', 'running')
        "#,
    )
    .bind(sha)
    .fetch_all(pool)
    .await?;

    Ok(statuses)
}

/// Insert a new GitLab commit status into the database.
///
/// This stores metadata about a commit status we've created via the GitLab API,
/// allowing us to update it later when build state changes.
pub async fn insert_commit_status_info(
    status_id: i64,
    sha: &str,
    name: &str,
    project_id: i64,
    domain: &str,
    repo_owner: &str,
    repo_name: &str,
    drv_id: i64,
    state: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        INSERT INTO GitLabCommitStatuses
            (status_id, sha, name, project_id, domain, repo_owner, repo_name, drv_id, state, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(status_id)
    .bind(sha)
    .bind(name)
    .bind(project_id)
    .bind(domain)
    .bind(repo_owner)
    .bind(repo_name)
    .bind(drv_id)
    .bind(state)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(())
}

/// Update the state of an existing commit status in the database.
///
/// Called when we update a status via the GitLab API to keep our local
/// tracking in sync.
pub async fn update_commit_status_state(
    status_id: i64,
    state: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        UPDATE GitLabCommitStatuses
        SET state = ?, updated_at = ?
        WHERE status_id = ?
        "#,
    )
    .bind(state)
    .bind(&now)
    .bind(status_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Upsert a GitLab merge request into the database.
///
/// If the MR already exists (same domain, project_id, mr_iid), it's updated.
/// Otherwise, a new row is inserted.
pub async fn upsert_merge_request(
    mr_iid: i64,
    owner: &str,
    repo_name: &str,
    project_id: i64,
    domain: &str,
    head_sha: &str,
    base_sha: &str,
    title: &str,
    author: &str,
    state: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        INSERT INTO GitLabMergeRequests
            (mr_iid, owner, repo_name, project_id, domain, head_sha, base_sha, title, author, state, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(domain, project_id, mr_iid) DO UPDATE SET
            head_sha = excluded.head_sha,
            base_sha = excluded.base_sha,
            title = excluded.title,
            author = excluded.author,
            state = excluded.state,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(mr_iid)
    .bind(owner)
    .bind(repo_name)
    .bind(project_id)
    .bind(domain)
    .bind(head_sha)
    .bind(base_sha)
    .bind(title)
    .bind(author)
    .bind(state)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get a merge request by its head commit SHA.
///
/// This is used to link commits to their associated MRs.
#[derive(Clone, Debug, FromRow)]
pub struct MergeRequestRow {
    pub mr_iid: i64,
    pub owner: String,
    pub repo_name: String,
    pub project_id: i64,
    pub domain: String,
    pub head_sha: String,
    pub base_sha: String,
    pub title: String,
    pub author: String,
    pub state: String,
    pub auto_merge_enabled: bool,
}

pub async fn get_mr_by_head_sha(
    head_sha: &str,
    project_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<MergeRequestRow>> {
    let mr = sqlx::query_as(
        r#"
        SELECT
            mr_iid, owner, repo_name, project_id, domain, head_sha, base_sha,
            title, author, state, auto_merge_enabled
        FROM GitLabMergeRequests
        WHERE head_sha = ? AND project_id = ?
        LIMIT 1
        "#,
    )
    .bind(head_sha)
    .bind(project_id)
    .fetch_optional(pool)
    .await?;

    Ok(mr)
}

/// Set comment-merge request for a merge request.
///
/// This is called when a user triggers a merge via MR comment.
/// Returns the number of rows affected.
pub async fn set_comment_merge(
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    sha: &str,
    method: Option<&str>,
    requester_id: i64,
    requester_username: &str,
    note_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<u64> {
    let now = chrono::Utc::now().to_rfc3339();

    let result = sqlx::query(
        r#"
        UPDATE GitLabMergeRequests
        SET
            comment_merge_sha = ?,
            comment_merge_method = ?,
            comment_merge_requester_id = ?,
            comment_merge_requester_username = ?,
            comment_merge_note_id = ?,
            comment_merge_requested_at = ?,
            updated_at = ?
        WHERE domain = ? AND project_id = ? AND mr_iid = ?
        "#,
    )
    .bind(sha)
    .bind(method)
    .bind(requester_id)
    .bind(requester_username)
    .bind(note_id)
    .bind(&now)
    .bind(&now)
    .bind(domain)
    .bind(project_id)
    .bind(mr_iid)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Clear comment-merge request for a merge request.
///
/// This is called after merge completes or when cancelled.
pub async fn clear_comment_merge(
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        UPDATE GitLabMergeRequests
        SET
            comment_merge_sha = NULL,
            comment_merge_method = NULL,
            comment_merge_requester_id = NULL,
            comment_merge_requester_username = NULL,
            comment_merge_note_id = NULL,
            comment_merge_requested_at = NULL,
            updated_at = ?
        WHERE domain = ? AND project_id = ? AND mr_iid = ?
        "#,
    )
    .bind(&now)
    .bind(domain)
    .bind(project_id)
    .bind(mr_iid)
    .execute(pool)
    .await?;

    Ok(())
}

/// Enable auto-merge for a merge request.
pub async fn enable_auto_merge(
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    merge_method: Option<&str>,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        UPDATE GitLabMergeRequests
        SET auto_merge_enabled = TRUE, merge_method = ?, updated_at = ?
        WHERE domain = ? AND project_id = ? AND mr_iid = ?
        "#,
    )
    .bind(merge_method)
    .bind(&now)
    .bind(domain)
    .bind(project_id)
    .bind(mr_iid)
    .execute(pool)
    .await?;

    Ok(())
}

/// Disable auto-merge for a merge request.
pub async fn disable_auto_merge(
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        UPDATE GitLabMergeRequests
        SET auto_merge_enabled = FALSE, merge_method = NULL, updated_at = ?
        WHERE domain = ? AND project_id = ? AND mr_iid = ?
        "#,
    )
    .bind(&now)
    .bind(domain)
    .bind(project_id)
    .bind(mr_iid)
    .execute(pool)
    .await?;

    Ok(())
}

/// Comment-merge request details
#[derive(Debug, Clone)]
pub struct CommentMergeRequest {
    pub sha: String,
    pub method: Option<String>,
    pub requester_id: i64,
    pub requester_username: String,
}

/// Full merge request row including comment-merge fields
#[derive(Clone, Debug, FromRow)]
pub struct MergeRequest {
    pub mr_iid: i64,
    pub owner: String,
    pub repo_name: String,
    pub project_id: i64,
    pub domain: String,
    pub head_sha: String,
    pub base_sha: String,
    pub title: String,
    pub author: String,
    pub state: String,
    pub auto_merge_enabled: bool,
    pub merge_method: Option<String>,
    pub comment_merge_sha: Option<String>,
    pub comment_merge_method: Option<String>,
    pub comment_merge_requester_id: Option<i64>,
    pub comment_merge_requester_username: Option<String>,
}

impl MergeRequest {
    /// Extract pending comment-merge request if present
    pub fn pending_comment_merge(&self) -> Option<CommentMergeRequest> {
        if let (Some(sha), Some(username)) = (
            self.comment_merge_sha.as_ref(),
            self.comment_merge_requester_username.as_ref(),
        ) {
            Some(CommentMergeRequest {
                sha: sha.clone(),
                method: self.comment_merge_method.clone(),
                requester_id: self.comment_merge_requester_id.unwrap_or(0),
                requester_username: username.clone(),
            })
        } else {
            None
        }
    }
}

/// Get a merge request by its identifiers, returning full row with comment-merge fields
pub async fn get_merge_request_row(
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<MergeRequest>> {
    let mr = sqlx::query_as(
        r#"
        SELECT
            mr_iid, owner, repo_name, project_id, domain, head_sha, base_sha,
            title, author, state, auto_merge_enabled, merge_method,
            comment_merge_sha, comment_merge_method, comment_merge_requester_id,
            comment_merge_requester_username
        FROM GitLabMergeRequests
        WHERE domain = ? AND project_id = ? AND mr_iid = ?
        "#,
    )
    .bind(domain)
    .bind(project_id)
    .bind(mr_iid)
    .fetch_optional(pool)
    .await?;

    Ok(mr)
}

/// Get all changed package attribute paths for a merge request
pub async fn get_mr_changed_packages(
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    pool: &Pool<Sqlite>,
) -> Result<Vec<String>> {
    // Get the MR's head_sha jobset
    let jobset_id: Option<i64> = sqlx::query_scalar(
        "SELECT gjs.ROWID FROM GitLabMergeRequests mr
         JOIN GitLabJobSets gjs ON mr.head_sha = gjs.sha
         WHERE mr.domain = ? AND mr.project_id = ? AND mr.mr_iid = ?
         AND gjs.domain = ? AND gjs.project_id = ?
         LIMIT 1",
    )
    .bind(domain)
    .bind(project_id)
    .bind(mr_iid)
    .bind(domain)
    .bind(project_id)
    .fetch_optional(pool)
    .await?;

    let Some(jobset_id) = jobset_id else {
        return Ok(vec![]);
    };

    // Get all unique attribute paths from job_difference for this jobset
    let attr_paths: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT attr_path FROM job_difference
         WHERE jobset = ?
         ORDER BY attr_path",
    )
    .bind(jobset_id)
    .fetch_all(pool)
    .await?;

    Ok(attr_paths)
}

/// Check if the merge request's head commit has successfully built
pub async fn mr_head_build_succeeded(
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    pool: &Pool<Sqlite>,
) -> Result<bool> {
    let jobset_id: Option<i64> = sqlx::query_scalar(
        "SELECT gjs.ROWID FROM GitLabMergeRequests mr
         JOIN GitLabJobSets gjs ON mr.head_sha = gjs.sha
         WHERE mr.domain = ? AND mr.project_id = ? AND mr.mr_iid = ?
         AND gjs.domain = ? AND gjs.project_id = ?
         LIMIT 1",
    )
    .bind(domain)
    .bind(project_id)
    .bind(mr_iid)
    .bind(domain)
    .bind(project_id)
    .fetch_optional(pool)
    .await?;

    let Some(jobset_id) = jobset_id else {
        return Ok(false);
    };

    if !crate::db::github::all_jobs_concluded(jobset_id, pool).await? {
        return Ok(false);
    }

    if crate::db::github::jobset_has_new_or_changed_failures(jobset_id, pool).await? {
        return Ok(false);
    }

    Ok(true)
}

/// Mark a merge request as merged
pub async fn mark_mr_merged(
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    merged_by_user_id: Option<i64>,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "UPDATE GitLabMergeRequests
         SET state = 'merged',
             merged_by_user_id = ?,
             merged_at = ?,
             comment_merge_sha = NULL,
             comment_merge_method = NULL,
             comment_merge_requester_id = NULL,
             comment_merge_requester_username = NULL,
             comment_merge_note_id = NULL,
             comment_merge_requested_at = NULL,
             updated_at = ?
         WHERE domain = ? AND project_id = ? AND mr_iid = ?",
    )
    .bind(merged_by_user_id)
    .bind(&now)
    .bind(&now)
    .bind(domain)
    .bind(project_id)
    .bind(mr_iid)
    .execute(pool)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbService;
    use crate::db::model::build_event::DrvBuildResult;

    #[tokio::test]
    async fn test_commit_status_insertion() {
        let db = DbService::new_in_memory().await.unwrap();
        let pool = &db.pool;

        // Insert a test derivation first
        sqlx::query(
            r#"
            INSERT INTO Drv (drv_path, system, required_system_features, is_fod, build_state)
            VALUES ('/nix/store/test.drv', 'x86_64-linux', '', 0, 7)
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let drv_rowid: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(pool)
            .await
            .unwrap();

        // Insert a commit status
        insert_commit_status_info(
            12345,
            "abc123",
            "eka-ci/build",
            100,
            "gitlab.com",
            "owner",
            "repo",
            drv_rowid,
            "pending",
            pool,
        )
        .await
        .unwrap();

        // Verify it was inserted
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM GitLabCommitStatuses WHERE status_id = 12345")
                .fetch_one(pool)
                .await
                .unwrap();

        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_commit_status_for_drv_path() {
        let db = DbService::new_in_memory().await.unwrap();
        let pool = &db.pool;

        // Insert a test derivation
        sqlx::query(
            r#"
            INSERT INTO Drv (drv_path, system, required_system_features, is_fod, build_state)
            VALUES ('/nix/store/test.drv', 'x86_64-linux', '', 0, 7)
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let drv_rowid: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(pool)
            .await
            .unwrap();

        let drv_path = DrvId::from("/nix/store/test.drv");

        // Insert a commit status
        insert_commit_status_info(
            12345,
            "abc123",
            "eka-ci/build",
            100,
            "gitlab.com",
            "owner",
            "repo",
            drv_rowid,
            "pending",
            pool,
        )
        .await
        .unwrap();

        // Query by drv_path
        let statuses = commit_statuses_for_drv_path(&drv_path, pool).await.unwrap();

        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].status_id, 12345);
        assert_eq!(statuses[0].name, "eka-ci/build");
        assert_eq!(statuses[0].build_state, DrvBuildState::Building);
    }

    #[tokio::test]
    async fn test_upsert_merge_request() {
        let db = DbService::new_in_memory().await.unwrap();
        let pool = &db.pool;

        // Insert a new MR
        upsert_merge_request(
            1,
            "owner",
            "repo",
            100,
            "gitlab.com",
            "abc123",
            "def456",
            "Test MR",
            "testuser",
            "opened",
            pool,
        )
        .await
        .unwrap();

        // Verify it was inserted
        let mr = get_mr_by_head_sha("abc123", 100, pool).await.unwrap();
        assert!(mr.is_some());
        let mr = mr.unwrap();
        assert_eq!(mr.mr_iid, 1);
        assert_eq!(mr.title, "Test MR");

        // Update the same MR
        upsert_merge_request(
            1,
            "owner",
            "repo",
            100,
            "gitlab.com",
            "abc123",
            "def456",
            "Updated Title",
            "testuser",
            "opened",
            pool,
        )
        .await
        .unwrap();

        // Verify it was updated (not duplicated)
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM GitLabMergeRequests WHERE domain = 'gitlab.com' AND project_id \
             = 100",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(count, 1);

        let mr = get_mr_by_head_sha("abc123", 100, pool).await.unwrap();
        assert!(mr.is_some());
        assert_eq!(mr.unwrap().title, "Updated Title");
    }

    #[tokio::test]
    async fn test_update_commit_status_state() {
        let db = DbService::new_in_memory().await.unwrap();
        let pool = &db.pool;

        // Insert a test derivation
        sqlx::query(
            r#"
            INSERT INTO Drv (drv_path, system, required_system_features, is_fod, build_state)
            VALUES ('/nix/store/test.drv', 'x86_64-linux', '', 0, 1)
            "#,
        )
        .execute(pool)
        .await
        .unwrap();

        let drv_rowid: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(pool)
            .await
            .unwrap();

        // Insert a commit status
        insert_commit_status_info(
            12345,
            "abc123",
            "eka-ci/build",
            100,
            "gitlab.com",
            "owner",
            "repo",
            drv_rowid,
            "pending",
            pool,
        )
        .await
        .unwrap();

        // Update the state
        update_commit_status_state(12345, "success", pool)
            .await
            .unwrap();

        // Verify it was updated
        let state: String =
            sqlx::query_scalar("SELECT state FROM GitLabCommitStatuses WHERE status_id = 12345")
                .fetch_one(pool)
                .await
                .unwrap();

        assert_eq!(state, "success");
    }
}
