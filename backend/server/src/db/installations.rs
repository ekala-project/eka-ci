use anyhow::Result;
use serde::Serialize;
use sqlx::{FromRow, Pool, Sqlite};

/// GitHub App installation information
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct GitHubInstallation {
    pub installation_id: i64,
    pub account_type: String,
    pub account_login: String,
    pub installed_at: String,
    pub updated_at: String,
    pub suspended_at: Option<String>,
}

/// Repository associated with a GitHub App installation
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct InstallationRepository {
    pub installation_id: i64,
    pub repo_id: i64,
    pub repo_name: String,
    pub repo_owner: String,
    pub added_at: String,
}

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

/// Check if a repository has the app installed
pub async fn is_repository_installed(
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<bool> {
    let exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM GitHubInstallationRepositories
            WHERE repo_owner = ? AND repo_name = ?
        )
        "#,
    )
    .bind(owner)
    .bind(repo_name)
    .fetch_one(pool)
    .await?;

    Ok(exists)
}

/// List all installed repositories
pub async fn list_installed_repositories(
    pool: &Pool<Sqlite>,
) -> Result<Vec<InstallationRepository>> {
    let repos = sqlx::query_as(
        r#"
        SELECT installation_id, repo_id, repo_name, repo_owner, added_at
        FROM GitHubInstallationRepositories
        ORDER BY repo_owner, repo_name
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(repos)
}

/// Get all repositories for a specific installation
pub async fn get_installation_repositories(
    installation_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<Vec<InstallationRepository>> {
    let repos = sqlx::query_as(
        r#"
        SELECT installation_id, repo_id, repo_name, repo_owner, added_at
        FROM GitHubInstallationRepositories
        WHERE installation_id = ?
        ORDER BY repo_owner, repo_name
        "#,
    )
    .bind(installation_id)
    .fetch_all(pool)
    .await?;

    Ok(repos)
}

/// Get an installation by ID
pub async fn get_installation(
    installation_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<GitHubInstallation>> {
    let installation = sqlx::query_as(
        r#"
        SELECT installation_id, account_type, account_login, installed_at, updated_at, suspended_at
        FROM GitHubInstallations
        WHERE installation_id = ?
        "#,
    )
    .bind(installation_id)
    .fetch_optional(pool)
    .await?;

    Ok(installation)
}
