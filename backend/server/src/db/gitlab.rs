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
