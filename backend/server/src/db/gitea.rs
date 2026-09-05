//! Database helper functions for Gitea platform operations.
//!
//! This module provides Gitea-specific database operations for check runs,
//! pull requests, and related data. Gitea supports both Check Runs (newer versions)
//! and Commit Statuses (older versions) for CI feedback.
use anyhow::Result;
use sqlx::{FromRow, Pool, Sqlite};

use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;

/// A Gitea check run linked to a derivation.
///
/// This struct represents a row from the GiteaCheckRuns table,
/// joining with the Drv table to get the current build state.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct CheckRun {
    /// Gitea API check run ID
    pub check_run_id: i64,
    /// Git commit SHA
    pub sha: String,
    /// Check run name (e.g., "eka-ci/build")
    pub name: String,
    /// Platform domain (e.g., "gitea.example.com")
    pub domain: String,
    /// Repository owner
    pub repo_owner: String,
    /// Repository name
    pub repo_name: String,
    /// Current build state of the associated derivation
    pub build_state: DrvBuildState,
    /// Derivation path
    pub drv_path: DrvId,
    /// Check run state: "pending", "running", "success", "failure", "cancelled"
    pub state: String,
}

/// Return all check runs which match a drv_path.
///
/// This is used when updating build status - we find all check runs
/// associated with a derivation and update them.
pub async fn check_runs_for_drv_path(
    drv_path: &DrvId,
    pool: &Pool<Sqlite>,
) -> Result<Vec<CheckRun>> {
    let check_runs = sqlx::query_as(
        r#"
        SELECT
            cr.check_run_id, cr.sha, cr.name, cr.domain,
            cr.repo_owner, cr.repo_name, d.build_state, d.drv_path,
            cr.state
        FROM GiteaCheckRuns cr
        INNER JOIN Drv d ON cr.drv_id = d.ROWID
        WHERE d.drv_path = ?
        "#,
    )
    .bind(drv_path)
    .fetch_all(pool)
    .await?;

    Ok(check_runs)
}

/// Return all check runs for a specific commit SHA that are still active
/// (not completed).
///
/// This is used when cancelling builds - we find all active check runs for a commit
/// and mark them as cancelled.
pub async fn check_runs_for_commit(sha: &str, pool: &Pool<Sqlite>) -> Result<Vec<CheckRun>> {
    let check_runs = sqlx::query_as(
        r#"
        SELECT DISTINCT
            cr.check_run_id, cr.sha, cr.name, cr.domain,
            cr.repo_owner, cr.repo_name, d.build_state, d.drv_path,
            cr.state
        FROM GiteaCheckRuns cr
        INNER JOIN Drv d ON cr.drv_id = d.ROWID
        INNER JOIN GiteaJob j ON j.drv_id = d.ROWID
        INNER JOIN GiteaJobSets g ON j.jobset = g.ROWID
        WHERE g.sha = ? AND cr.state NOT IN ('success', 'failure', 'cancelled')
        "#,
    )
    .bind(sha)
    .fetch_all(pool)
    .await?;

    Ok(check_runs)
}

/// Insert a new Gitea check run into the database.
///
/// This stores metadata about a check run we've created via the Gitea API,
/// allowing us to update it later when build state changes.
pub async fn insert_check_run_info(
    check_run_id: i64,
    sha: &str,
    name: &str,
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
        INSERT INTO GiteaCheckRuns
            (check_run_id, sha, name, domain, repo_owner, repo_name, drv_id, state, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(check_run_id)
    .bind(sha)
    .bind(name)
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

/// Update the state of an existing check run in the database.
///
/// Called when we update a check run via the Gitea API to keep our local
/// tracking in sync.
pub async fn update_check_run_status(
    check_run_id: i64,
    state: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        UPDATE GiteaCheckRuns
        SET state = ?, updated_at = ?
        WHERE check_run_id = ?
        "#,
    )
    .bind(state)
    .bind(&now)
    .bind(check_run_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Upsert a Gitea pull request into the database.
///
/// If the PR already exists (same domain, owner, repo_name, pr_number), it's updated.
/// Otherwise, a new row is inserted.
pub async fn upsert_pull_request(
    pr_number: i64,
    owner: &str,
    repo_name: &str,
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
        INSERT INTO GiteaPullRequests
            (pr_number, owner, repo_name, domain, head_sha, base_sha, title, author, state, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(domain, owner, repo_name, pr_number) DO UPDATE SET
            head_sha = excluded.head_sha,
            base_sha = excluded.base_sha,
            title = excluded.title,
            author = excluded.author,
            state = excluded.state,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(pr_number)
    .bind(owner)
    .bind(repo_name)
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

/// Get a pull request by its head commit SHA.
///
/// This is used to link commits to their associated PRs.
#[allow(dead_code)]
#[derive(Clone, Debug, FromRow)]
pub struct PullRequestRow {
    pub pr_number: i64,
    pub owner: String,
    pub repo_name: String,
    pub domain: String,
    pub head_sha: String,
    pub base_sha: String,
    pub title: String,
    pub author: String,
    pub state: String,
    pub auto_merge_enabled: bool,
}

pub async fn get_pr_by_head_sha(
    head_sha: &str,
    domain: &str,
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<PullRequestRow>> {
    let pr = sqlx::query_as(
        r#"
        SELECT
            pr_number, owner, repo_name, domain, head_sha, base_sha,
            title, author, state, auto_merge_enabled
        FROM GiteaPullRequests
        WHERE head_sha = ? AND domain = ? AND owner = ? AND repo_name = ?
        LIMIT 1
        "#,
    )
    .bind(head_sha)
    .bind(domain)
    .bind(owner)
    .bind(repo_name)
    .fetch_optional(pool)
    .await?;

    Ok(pr)
}

/// Set comment-merge request for a pull request.
///
/// This is called when a user triggers a merge via PR comment.
/// Returns the number of rows affected.
pub async fn set_comment_merge(
    domain: &str,
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
    let now = chrono::Utc::now().to_rfc3339();

    let result = sqlx::query(
        r#"
        UPDATE GiteaPullRequests
        SET
            comment_merge_sha = ?,
            comment_merge_method = ?,
            comment_merge_requester_id = ?,
            comment_merge_requester_login = ?,
            comment_merge_comment_id = ?,
            comment_merge_requested_at = ?,
            updated_at = ?
        WHERE domain = ? AND owner = ? AND repo_name = ? AND pr_number = ?
        "#,
    )
    .bind(sha)
    .bind(method)
    .bind(requester_id)
    .bind(requester_login)
    .bind(comment_id)
    .bind(&now)
    .bind(&now)
    .bind(domain)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Clear comment-merge request for a pull request.
///
/// This is called after merge completes or when cancelled.
pub async fn clear_comment_merge(
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        UPDATE GiteaPullRequests
        SET
            comment_merge_sha = NULL,
            comment_merge_method = NULL,
            comment_merge_requester_id = NULL,
            comment_merge_requester_login = NULL,
            comment_merge_comment_id = NULL,
            comment_merge_requested_at = NULL,
            updated_at = ?
        WHERE domain = ? AND owner = ? AND repo_name = ? AND pr_number = ?
        "#,
    )
    .bind(&now)
    .bind(domain)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;

    Ok(())
}

/// Enable auto-merge for a pull request.
#[allow(dead_code)]
pub async fn enable_auto_merge(
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    merge_method: Option<&str>,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        UPDATE GiteaPullRequests
        SET auto_merge_enabled = TRUE, merge_method = ?, updated_at = ?
        WHERE domain = ? AND owner = ? AND repo_name = ? AND pr_number = ?
        "#,
    )
    .bind(merge_method)
    .bind(&now)
    .bind(domain)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;

    Ok(())
}

/// Disable auto-merge for a pull request.
#[allow(dead_code)]
pub async fn disable_auto_merge(
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        UPDATE GiteaPullRequests
        SET auto_merge_enabled = FALSE, merge_method = NULL, updated_at = ?
        WHERE domain = ? AND owner = ? AND repo_name = ? AND pr_number = ?
        "#,
    )
    .bind(&now)
    .bind(domain)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
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
    pub requester_login: String,
}

/// Full pull request row including comment-merge fields
#[allow(dead_code)]
#[derive(Clone, Debug, FromRow)]
pub struct PullRequest {
    pub pr_number: i64,
    pub owner: String,
    pub repo_name: String,
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
    pub comment_merge_requester_login: Option<String>,
}

impl PullRequest {
    /// Extract pending comment-merge request if present
    pub fn pending_comment_merge(&self) -> Option<CommentMergeRequest> {
        if let (Some(sha), Some(login)) = (
            self.comment_merge_sha.as_ref(),
            self.comment_merge_requester_login.as_ref(),
        ) {
            Some(CommentMergeRequest {
                sha: sha.clone(),
                method: self.comment_merge_method.clone(),
                requester_id: self.comment_merge_requester_id.unwrap_or(0),
                requester_login: login.clone(),
            })
        } else {
            None
        }
    }
}

/// Get a pull request by its identifiers, returning full row with comment-merge fields
pub async fn get_pull_request_row(
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<PullRequest>> {
    let pr = sqlx::query_as(
        r#"
        SELECT
            pr_number, owner, repo_name, domain, head_sha, base_sha,
            title, author, state, auto_merge_enabled, merge_method,
            comment_merge_sha, comment_merge_method, comment_merge_requester_id,
            comment_merge_requester_login
        FROM GiteaPullRequests
        WHERE domain = ? AND owner = ? AND repo_name = ? AND pr_number = ?
        "#,
    )
    .bind(domain)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .fetch_optional(pool)
    .await?;

    Ok(pr)
}

/// Get all changed package attribute paths for a pull request
pub async fn get_pr_changed_packages(
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<Vec<String>> {
    // Get the PR's head_sha jobset
    let jobset_id: Option<i64> = sqlx::query_scalar(
        "SELECT gjs.ROWID FROM GiteaPullRequests pr
         JOIN GiteaJobSets gjs ON pr.head_sha = gjs.sha
         WHERE pr.domain = ? AND pr.owner = ? AND pr.repo_name = ? AND pr.pr_number = ?
         AND gjs.domain = ? AND gjs.owner = ? AND gjs.repo_name = ?
         LIMIT 1",
    )
    .bind(domain)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .bind(domain)
    .bind(owner)
    .bind(repo_name)
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

/// Check if the pull request's head commit has successfully built
pub async fn pr_head_build_succeeded(
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<bool> {
    let jobset_id: Option<i64> = sqlx::query_scalar(
        "SELECT gjs.ROWID FROM GiteaPullRequests pr
         JOIN GiteaJobSets gjs ON pr.head_sha = gjs.sha
         WHERE pr.domain = ? AND pr.owner = ? AND pr.repo_name = ? AND pr.pr_number = ?
         AND gjs.domain = ? AND gjs.owner = ? AND gjs.repo_name = ?
         LIMIT 1",
    )
    .bind(domain)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .bind(domain)
    .bind(owner)
    .bind(repo_name)
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

/// Mark a pull request as merged
pub async fn mark_pr_merged(
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    merged_by_user_id: Option<i64>,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "UPDATE GiteaPullRequests
         SET state = 'merged',
             merged_by_user_id = ?,
             merged_at = ?,
             comment_merge_sha = NULL,
             comment_merge_method = NULL,
             comment_merge_requester_id = NULL,
             comment_merge_requester_login = NULL,
             comment_merge_comment_id = NULL,
             comment_merge_requested_at = NULL,
             updated_at = ?
         WHERE domain = ? AND owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(merged_by_user_id)
    .bind(&now)
    .bind(&now)
    .bind(domain)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbService;

    #[tokio::test]
    async fn test_check_run_insertion() {
        let db = DbService::new_in_memory().await.unwrap();
        let pool = &db.pool;

        // Insert a test derivation first (use a valid drv format: 32-char hash + name.drv)
        let test_drv = "0000000000000000000000000000test-test.drv";
        sqlx::query(
            r#"
            INSERT INTO Drv (drv_path, system, required_system_features, is_fod, build_state)
            VALUES (?, 'x86_64-linux', '', 0, 7)
            "#,
        )
        .bind(test_drv)
        .execute(pool)
        .await
        .unwrap();

        let drv_rowid: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(pool)
            .await
            .unwrap();

        // Insert a check run
        insert_check_run_info(
            12345,
            "abc123",
            "eka-ci/build",
            "gitea.example.com",
            "owner",
            "repo",
            drv_rowid,
            "running",
            pool,
        )
        .await
        .unwrap();

        // Verify it was inserted
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM GiteaCheckRuns WHERE check_run_id = 12345")
                .fetch_one(pool)
                .await
                .unwrap();

        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_check_runs_for_drv_path() {
        let db = DbService::new_in_memory().await.unwrap();
        let pool = &db.pool;

        // Insert a test derivation (DrvId stores just the filename, not the full path)
        let test_drv_filename = "00000000000000000000000000000000-test.drv";
        sqlx::query(
            r#"
            INSERT INTO Drv (drv_path, system, required_system_features, is_fod, build_state)
            VALUES (?, 'x86_64-linux', '', 0, 7)
            "#,
        )
        .bind(test_drv_filename)
        .execute(pool)
        .await
        .unwrap();

        let drv_rowid: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(pool)
            .await
            .unwrap();

        let drv_path =
            DrvId::try_from("/nix/store/00000000000000000000000000000000-test.drv").unwrap();

        // Insert a check run
        insert_check_run_info(
            12345,
            "abc123",
            "eka-ci/build",
            "gitea.example.com",
            "owner",
            "repo",
            drv_rowid,
            "running",
            pool,
        )
        .await
        .unwrap();

        // Query by drv_path
        let check_runs = check_runs_for_drv_path(&drv_path, pool).await.unwrap();

        assert_eq!(check_runs.len(), 1);
        assert_eq!(check_runs[0].check_run_id, 12345);
        assert_eq!(check_runs[0].name, "eka-ci/build");
        assert_eq!(check_runs[0].build_state, DrvBuildState::Building);
    }

    #[tokio::test]
    async fn test_upsert_pull_request() {
        let db = DbService::new_in_memory().await.unwrap();
        let pool = &db.pool;

        // Insert a new PR
        upsert_pull_request(
            1,
            "owner",
            "repo",
            "gitea.example.com",
            "abc123",
            "def456",
            "Test PR",
            "testuser",
            "open",
            pool,
        )
        .await
        .unwrap();

        // Verify it was inserted
        let pr = get_pr_by_head_sha("abc123", "gitea.example.com", "owner", "repo", pool)
            .await
            .unwrap();
        assert!(pr.is_some());
        let pr = pr.unwrap();
        assert_eq!(pr.pr_number, 1);
        assert_eq!(pr.title, "Test PR");

        // Update the same PR
        upsert_pull_request(
            1,
            "owner",
            "repo",
            "gitea.example.com",
            "abc123",
            "def456",
            "Updated Title",
            "testuser",
            "open",
            pool,
        )
        .await
        .unwrap();

        // Verify it was updated (not duplicated)
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM GiteaPullRequests WHERE domain = 'gitea.example.com' AND owner \
             = 'owner' AND repo_name = 'repo'",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(count, 1);

        let pr = get_pr_by_head_sha("abc123", "gitea.example.com", "owner", "repo", pool)
            .await
            .unwrap();
        assert!(pr.is_some());
        assert_eq!(pr.unwrap().title, "Updated Title");
    }

    #[tokio::test]
    async fn test_update_check_run_status() {
        let db = DbService::new_in_memory().await.unwrap();
        let pool = &db.pool;

        // Insert a test derivation (use a valid drv format)
        let test_drv = "0000000000000000000000000000test-test.drv";
        sqlx::query(
            r#"
            INSERT INTO Drv (drv_path, system, required_system_features, is_fod, build_state)
            VALUES (?, 'x86_64-linux', '', 0, 1)
            "#,
        )
        .bind(test_drv)
        .execute(pool)
        .await
        .unwrap();

        let drv_rowid: i64 = sqlx::query_scalar("SELECT last_insert_rowid()")
            .fetch_one(pool)
            .await
            .unwrap();

        // Insert a check run
        insert_check_run_info(
            12345,
            "abc123",
            "eka-ci/build",
            "gitea.example.com",
            "owner",
            "repo",
            drv_rowid,
            "running",
            pool,
        )
        .await
        .unwrap();

        // Update the state
        update_check_run_status(12345, "success", pool)
            .await
            .unwrap();

        // Verify it was updated
        let state: String =
            sqlx::query_scalar("SELECT state FROM GiteaCheckRuns WHERE check_run_id = 12345")
                .fetch_one(pool)
                .await
                .unwrap();

        assert_eq!(state, "success");
    }
}
