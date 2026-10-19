use anyhow::Result;
use sqlx::{Pool, Sqlite};

/// Insert a skill evaluation result.
pub async fn insert_skill_result(
    checkset_id: i64,
    success: bool,
    error: bool,
    reason: &str,
    findings: &str,
    model_id: &str,
    duration_ms: i64,
    pool: &Pool<Sqlite>,
) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO SkillResult (checkset_id, success, error, reason, findings, model_id, \
         duration_ms) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(checkset_id)
    .bind(success)
    .bind(error)
    .bind(reason)
    .bind(findings)
    .bind(model_id)
    .bind(duration_ms)
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}
