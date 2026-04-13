use anyhow::Result;
use serde::Serialize;
use sqlx::{FromRow, Pool, Sqlite};

/// Information about a check run linked to a check result
#[derive(Clone, Debug, PartialEq, Eq, FromRow, Serialize)]
pub struct CheckRunInfo {
    pub check_run_id: i64,
    pub check_result_id: i64,
    pub repo_name: String,
    pub repo_owner: String,
}

/// Insert a new GitHub checkset
pub async fn insert_github_checkset(
    sha: &str,
    check_name: &str,
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<i64> {
    sqlx::query(
        "INSERT INTO GitHubCheckSets (sha, check_name, owner, repo_name) VALUES (?, ?, ?, ?)",
    )
    .bind(sha)
    .bind(check_name)
    .bind(owner)
    .bind(repo_name)
    .execute(pool)
    .await?;

    let checkset_id = sqlx::query_scalar(
        "SELECT ROWID FROM GitHubCheckSets WHERE sha = ? AND check_name = ? AND owner = ? AND \
         repo_name = ?",
    )
    .bind(sha)
    .bind(check_name)
    .bind(owner)
    .bind(repo_name)
    .fetch_one(pool)
    .await?;

    Ok(checkset_id)
}

/// Insert a check result
pub async fn insert_check_result(
    checkset_id: i64,
    success: bool,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    duration_ms: i64,
    pool: &Pool<Sqlite>,
) -> Result<i64> {
    let result = sqlx::query(
        "INSERT INTO CheckResult (checkset, success, exit_code, stdout, stderr, duration_ms) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(checkset_id)
    .bind(success)
    .bind(exit_code)
    .bind(stdout)
    .bind(stderr)
    .bind(duration_ms)
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

/// Insert check run info linking GitHub check run ID to check result
pub async fn insert_check_run_info(
    check_run_id: i64,
    check_result_id: i64,
    repo_name: &str,
    repo_owner: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO CheckRunInfo (check_run_id, check_result_id, repo_name, repo_owner) VALUES \
         (?, ?, ?, ?)",
    )
    .bind(check_run_id)
    .bind(check_result_id)
    .bind(repo_name)
    .bind(repo_owner)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get check run info by checkset ID
pub async fn get_check_run_info_by_checkset(
    owner: &str,
    repo_name: &str,
    checkset_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<CheckRunInfo>> {
    let result = sqlx::query_as::<_, CheckRunInfo>(
        r#"
        SELECT cri.check_run_id, cri.check_result_id, cri.repo_name, cri.repo_owner
        FROM CheckRunInfo cri
        INNER JOIN CheckResult cr ON cr.ROWID = cri.check_result_id
        WHERE cr.checkset = ? AND cri.repo_owner = ? AND cri.repo_name = ?
        LIMIT 1
        "#,
    )
    .bind(checkset_id)
    .bind(owner)
    .bind(repo_name)
    .fetch_optional(pool)
    .await?;

    Ok(result)
}
