//! Database operations for hook executions

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;

/// Hook execution record stored in the database
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct HookExecution {
    pub id: i64,
    pub drv_path: String,
    pub hook_name: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub exit_code: Option<i32>,
    pub success: bool,
    pub log_path: String,
}

/// Insert a hook execution record into the database
pub async fn insert_hook_execution(
    pool: &SqlitePool,
    drv_path: &str,
    hook_name: &str,
    started_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
    exit_code: Option<i32>,
    success: bool,
    log_path: &str,
) -> anyhow::Result<i64> {
    let result = sqlx::query(
        r#"
        INSERT INTO HookExecution (drv_path, hook_name, started_at, completed_at, exit_code, success, log_path)
        VALUES (?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(drv_path)
    .bind(hook_name)
    .bind(started_at)
    .bind(completed_at)
    .bind(exit_code)
    .bind(success)
    .bind(log_path)
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

/// Get all hook executions for a specific drv
pub async fn get_hook_executions_for_drv(
    pool: &SqlitePool,
    drv_path: &str,
) -> anyhow::Result<Vec<HookExecution>> {
    let executions = sqlx::query_as::<_, HookExecution>(
        r#"
        SELECT id, drv_path, hook_name, started_at, completed_at, exit_code, success, log_path
        FROM HookExecution
        WHERE drv_path = ?
        ORDER BY started_at DESC
        "#,
    )
    .bind(drv_path)
    .fetch_all(pool)
    .await?;

    Ok(executions)
}

/// Get a specific hook execution by drv and hook name (most recent)
pub async fn get_hook_execution(
    pool: &SqlitePool,
    drv_path: &str,
    hook_name: &str,
) -> anyhow::Result<Option<HookExecution>> {
    let execution = sqlx::query_as::<_, HookExecution>(
        r#"
        SELECT id, drv_path, hook_name, started_at, completed_at, exit_code, success, log_path
        FROM HookExecution
        WHERE drv_path = ? AND hook_name = ?
        ORDER BY started_at DESC
        LIMIT 1
        "#,
    )
    .bind(drv_path)
    .bind(hook_name)
    .fetch_optional(pool)
    .await?;

    Ok(execution)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    async fn setup_test_db() -> SqlitePool {
        let pool = SqlitePool::connect(":memory:").await.unwrap();

        // Create the HookExecution table
        sqlx::query(
            r#"
            CREATE TABLE HookExecution (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                drv_path TEXT NOT NULL,
                hook_name TEXT NOT NULL,
                started_at TEXT NOT NULL,
                completed_at TEXT,
                exit_code INTEGER,
                success BOOLEAN NOT NULL,
                log_path TEXT NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        pool
    }

    #[tokio::test]
    async fn test_insert_and_get_hook_execution() {
        let pool = setup_test_db().await;
        let drv_path = "/nix/store/abc123-test.drv";
        let hook_name = "upload-to-s3";
        let started_at = Utc::now();
        let completed_at = Utc::now();
        let log_path = "/logs/abc123-test/hook-upload-to-s3.log";

        // Insert hook execution
        let id = insert_hook_execution(
            &pool,
            drv_path,
            hook_name,
            started_at,
            completed_at,
            Some(0),
            true,
            log_path,
        )
        .await
        .unwrap();

        assert!(id > 0);

        // Retrieve hook execution
        let execution = get_hook_execution(&pool, drv_path, hook_name)
            .await
            .unwrap()
            .expect("Hook execution should exist");

        assert_eq!(execution.drv_path, drv_path);
        assert_eq!(execution.hook_name, hook_name);
        assert_eq!(execution.exit_code, Some(0));
        assert!(execution.success);
        assert_eq!(execution.log_path, log_path);
    }

    #[tokio::test]
    async fn test_get_hook_executions_for_drv() {
        let pool = setup_test_db().await;
        let drv_path = "/nix/store/abc123-test.drv";
        let now = Utc::now();
        let later = now + chrono::Duration::seconds(1);

        // Insert multiple hook executions
        insert_hook_execution(
            &pool,
            drv_path,
            "hook1",
            now,
            now,
            Some(0),
            true,
            "/logs/abc123-test/hook-hook1.log",
        )
        .await
        .unwrap();

        insert_hook_execution(
            &pool,
            drv_path,
            "hook2",
            later,
            later,
            Some(1),
            false,
            "/logs/abc123-test/hook-hook2.log",
        )
        .await
        .unwrap();

        // Retrieve all hook executions for drv
        let executions = get_hook_executions_for_drv(&pool, drv_path).await.unwrap();

        assert_eq!(executions.len(), 2);
        assert_eq!(executions[0].hook_name, "hook2"); // Most recent first
        assert_eq!(executions[1].hook_name, "hook1");
    }

    #[tokio::test]
    async fn test_get_nonexistent_hook_execution() {
        let pool = setup_test_db().await;

        let execution = get_hook_execution(&pool, "/nix/store/nonexistent.drv", "hook1")
            .await
            .unwrap();

        assert!(execution.is_none());
    }
}
