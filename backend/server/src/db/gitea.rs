///! Database helper functions for Gitea platform operations.
///!
///! This module provides Gitea-specific database operations for check runs,
///! pull requests, and related data. Gitea supports both Check Runs (newer versions)
///! and Commit Statuses (older versions) for CI feedback.
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
    /// Check run status: "queued", "in_progress", "completed"
    pub status: String,
    /// Check run conclusion (if completed): "success", "failure", "cancelled", etc.
    pub conclusion: Option<String>,
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
            cr.status, cr.conclusion
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
            cr.status, cr.conclusion
        FROM GiteaCheckRuns cr
        INNER JOIN Drv d ON cr.drv_id = d.ROWID
        INNER JOIN GiteaJob j ON j.drv_id = d.ROWID
        INNER JOIN GiteaJobSets g ON j.jobset = g.ROWID
        WHERE g.sha = ? AND cr.status != 'completed'
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
    status: &str,
    conclusion: Option<&str>,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        INSERT INTO GiteaCheckRuns
            (check_run_id, sha, name, domain, repo_owner, repo_name, drv_id, status, conclusion, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(check_run_id)
    .bind(sha)
    .bind(name)
    .bind(domain)
    .bind(repo_owner)
    .bind(repo_name)
    .bind(drv_id)
    .bind(status)
    .bind(conclusion)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(())
}

/// Update the status and conclusion of an existing check run in the database.
///
/// Called when we update a check run via the Gitea API to keep our local
/// tracking in sync.
pub async fn update_check_run_status(
    check_run_id: i64,
    status: &str,
    conclusion: Option<&str>,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        UPDATE GiteaCheckRuns
        SET status = ?, conclusion = ?, updated_at = ?
        WHERE check_run_id = ?
        "#,
    )
    .bind(status)
    .bind(conclusion)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbService;

    #[tokio::test]
    async fn test_check_run_insertion() {
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

        // Insert a check run
        insert_check_run_info(
            12345,
            "abc123",
            "eka-ci/build",
            "gitea.example.com",
            "owner",
            "repo",
            drv_rowid,
            "in_progress",
            None,
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

        // Insert a check run
        insert_check_run_info(
            12345,
            "abc123",
            "eka-ci/build",
            "gitea.example.com",
            "owner",
            "repo",
            drv_rowid,
            "in_progress",
            None,
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

        // Insert a check run
        insert_check_run_info(
            12345,
            "abc123",
            "eka-ci/build",
            "gitea.example.com",
            "owner",
            "repo",
            drv_rowid,
            "in_progress",
            None,
            pool,
        )
        .await
        .unwrap();

        // Update the status
        update_check_run_status(12345, "completed", Some("success"), pool)
            .await
            .unwrap();

        // Verify it was updated
        let (status, conclusion): (String, Option<String>) = sqlx::query_as(
            "SELECT status, conclusion FROM GiteaCheckRuns WHERE check_run_id = 12345",
        )
        .fetch_one(pool)
        .await
        .unwrap();

        assert_eq!(status, "completed");
        assert_eq!(conclusion.as_deref(), Some("success"));
    }
}
