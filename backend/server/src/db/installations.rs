use anyhow::Result;
use sqlx::{Pool, Sqlite};

/// Upsert a GitHub App installation
/// Updates the record if it exists, inserts if it doesn't
pub async fn upsert_installation(
    installation_id: i64,
    account_type: &str,
    account_login: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        INSERT INTO GitHubInstallations (installation_id, account_type, account_login, installed_at, updated_at)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(installation_id) DO UPDATE SET
            account_type = excluded.account_type,
            account_login = excluded.account_login,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(installation_id)
    .bind(account_type)
    .bind(account_login)
    .bind(&now)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark an installation as suspended
pub async fn suspend_installation(installation_id: i64, pool: &Pool<Sqlite>) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        UPDATE GitHubInstallations
        SET suspended_at = ?, updated_at = ?
        WHERE installation_id = ?
        "#,
    )
    .bind(&now)
    .bind(&now)
    .bind(installation_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark an installation as unsuspended
pub async fn unsuspend_installation(installation_id: i64, pool: &Pool<Sqlite>) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        UPDATE GitHubInstallations
        SET suspended_at = NULL, updated_at = ?
        WHERE installation_id = ?
        "#,
    )
    .bind(&now)
    .bind(installation_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Delete an installation (cascades to repositories)
pub async fn delete_installation(installation_id: i64, pool: &Pool<Sqlite>) -> Result<()> {
    sqlx::query(
        r#"
        DELETE FROM GitHubInstallations
        WHERE installation_id = ?
        "#,
    )
    .bind(installation_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Add or update a repository in an installation
pub async fn upsert_installation_repository(
    installation_id: i64,
    repo_id: i64,
    repo_name: &str,
    repo_owner: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        r#"
        INSERT INTO GitHubInstallationRepositories (installation_id, repo_id, repo_name, repo_owner, added_at)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(installation_id, repo_id) DO UPDATE SET
            repo_name = excluded.repo_name,
            repo_owner = excluded.repo_owner
        "#,
    )
    .bind(installation_id)
    .bind(repo_id)
    .bind(repo_name)
    .bind(repo_owner)
    .bind(&now)
    .execute(pool)
    .await?;

    Ok(())
}

/// Remove a repository from an installation
pub async fn delete_installation_repository(
    installation_id: i64,
    repo_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        r#"
        DELETE FROM GitHubInstallationRepositories
        WHERE installation_id = ? AND repo_id = ?
        "#,
    )
    .bind(installation_id)
    .bind(repo_id)
    .execute(pool)
    .await?;

    Ok(())
}
