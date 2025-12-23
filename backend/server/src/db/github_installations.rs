use anyhow::Result;
use octocrab::models::Installation;
use sqlx::{FromRow, Pool, Sqlite};

#[derive(Clone, Debug, FromRow)]
pub struct GitHubInstallation {
    pub installation_id: i64,
    pub account_id: i64,
    pub account_login: String,
    pub account_type: String,
    pub repository_selection: String,
    pub selected_repositories_count: Option<i64>,
    pub suspended_at: Option<String>, // SQLite stores as TEXT
    pub created_at: String,           // SQLite stores as TEXT
    pub updated_at: String,           // SQLite stores as TEXT
}

/// Convert an octocrab Installation to our database representation
impl From<&Installation> for GitHubInstallation {
    fn from(installation: &Installation) -> Self {
        let account_type = match installation.account.r#type.as_str() {
            "User" => "User".to_string(),
            "Organization" => "Organization".to_string(),
            _ => "Unknown".to_string(),
        };

        let repository_selection = installation
            .repository_selection
            .as_ref()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "all".to_string());

        Self {
            installation_id: installation.id.0 as i64,
            account_id: installation.account.id.0 as i64,
            account_login: installation.account.login.clone(),
            account_type,
            repository_selection,
            selected_repositories_count: None, // Not available in basic Installation type
            suspended_at: None,                // Set via webhook events
            created_at: installation
                .created_at
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| "CURRENT_TIMESTAMP".to_string()),
            updated_at: installation
                .updated_at
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_else(|| "CURRENT_TIMESTAMP".to_string()),
        }
    }
}

/// Insert or update a GitHub installation
/// Uses REPLACE to handle both insert and update cases
pub async fn upsert_installation(installation: &Installation, pool: &Pool<Sqlite>) -> Result<()> {
    let db_installation = GitHubInstallation::from(installation);

    sqlx::query(
        "INSERT INTO GitHubInstallations
         (installation_id, account_id, account_login, account_type, repository_selection,
          selected_repositories_count, suspended_at, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(installation_id) DO UPDATE SET
            account_id = excluded.account_id,
            account_login = excluded.account_login,
            account_type = excluded.account_type,
            repository_selection = excluded.repository_selection,
            selected_repositories_count = excluded.selected_repositories_count,
            updated_at = CURRENT_TIMESTAMP",
    )
    .bind(db_installation.installation_id)
    .bind(db_installation.account_id)
    .bind(db_installation.account_login)
    .bind(db_installation.account_type)
    .bind(db_installation.repository_selection)
    .bind(db_installation.selected_repositories_count)
    .bind(db_installation.suspended_at)
    .bind(db_installation.created_at)
    .bind(db_installation.updated_at)
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark an installation as suspended
pub async fn suspend_installation(installation_id: i64, pool: &Pool<Sqlite>) -> Result<()> {
    sqlx::query(
        "UPDATE GitHubInstallations
         SET suspended_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP
         WHERE installation_id = ?",
    )
    .bind(installation_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Mark an installation as unsuspended (clear suspended_at)
pub async fn unsuspend_installation(installation_id: i64, pool: &Pool<Sqlite>) -> Result<()> {
    sqlx::query(
        "UPDATE GitHubInstallations
         SET suspended_at = NULL, updated_at = CURRENT_TIMESTAMP
         WHERE installation_id = ?",
    )
    .bind(installation_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Delete an installation from the database
pub async fn delete_installation(installation_id: i64, pool: &Pool<Sqlite>) -> Result<()> {
    sqlx::query("DELETE FROM GitHubInstallations WHERE installation_id = ?")
        .bind(installation_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Get an installation by its installation_id
pub async fn get_installation_by_id(
    installation_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<GitHubInstallation>> {
    let installation = sqlx::query_as(
        "SELECT installation_id, account_id, account_login, account_type, repository_selection,
                selected_repositories_count, suspended_at, created_at, updated_at
         FROM GitHubInstallations
         WHERE installation_id = ?",
    )
    .bind(installation_id)
    .fetch_optional(pool)
    .await?;

    Ok(installation)
}

/// Get an installation by account login
pub async fn get_installation_by_login(
    account_login: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<GitHubInstallation>> {
    let installation = sqlx::query_as(
        "SELECT installation_id, account_id, account_login, account_type, repository_selection,
                selected_repositories_count, suspended_at, created_at, updated_at
         FROM GitHubInstallations
         WHERE account_login = ?",
    )
    .bind(account_login)
    .fetch_optional(pool)
    .await?;

    Ok(installation)
}

/// List all installations (including suspended ones)
pub async fn list_all_installations(pool: &Pool<Sqlite>) -> Result<Vec<GitHubInstallation>> {
    let installations = sqlx::query_as(
        "SELECT installation_id, account_id, account_login, account_type, repository_selection,
                selected_repositories_count, suspended_at, created_at, updated_at
         FROM GitHubInstallations
         ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await?;

    Ok(installations)
}

/// List only active (non-suspended) installations
pub async fn list_active_installations(pool: &Pool<Sqlite>) -> Result<Vec<GitHubInstallation>> {
    let installations = sqlx::query_as(
        "SELECT installation_id, account_id, account_login, account_type, repository_selection,
                selected_repositories_count, suspended_at, created_at, updated_at
         FROM GitHubInstallations
         WHERE suspended_at IS NULL
         ORDER BY created_at DESC",
    )
    .fetch_all(pool)
    .await?;

    Ok(installations)
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_suspend_unsuspend(pool: SqlitePool) -> anyhow::Result<()> {
        // Manually insert a test installation
        sqlx::query(
            "INSERT INTO GitHubInstallations
             (installation_id, account_id, account_login, account_type, repository_selection)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(1002)
        .bind(12345)
        .bind("testuser")
        .bind("User")
        .bind("all")
        .execute(&pool)
        .await?;

        // Initially not suspended
        let retrieved = get_installation_by_id(1002, &pool).await?.unwrap();
        assert!(retrieved.suspended_at.is_none());

        // Suspend the installation
        suspend_installation(1002, &pool).await?;
        let retrieved = get_installation_by_id(1002, &pool).await?.unwrap();
        assert!(retrieved.suspended_at.is_some());

        // Unsuspend the installation
        unsuspend_installation(1002, &pool).await?;
        let retrieved = get_installation_by_id(1002, &pool).await?.unwrap();
        assert!(retrieved.suspended_at.is_none());

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_delete_installation(pool: SqlitePool) -> anyhow::Result<()> {
        // Manually insert a test installation
        sqlx::query(
            "INSERT INTO GitHubInstallations
             (installation_id, account_id, account_login, account_type, repository_selection)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(1003)
        .bind(12345)
        .bind("deletetest")
        .bind("User")
        .bind("all")
        .execute(&pool)
        .await?;

        // Verify it exists
        assert!(get_installation_by_id(1003, &pool).await?.is_some());

        // Delete it
        delete_installation(1003, &pool).await?;

        // Verify it's gone
        assert!(get_installation_by_id(1003, &pool).await?.is_none());

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_list_installations(pool: SqlitePool) -> anyhow::Result<()> {
        // Insert two test installations
        for (id, login) in [(2001, "org1"), (2002, "org2")] {
            sqlx::query(
                "INSERT INTO GitHubInstallations
                 (installation_id, account_id, account_login, account_type, repository_selection)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .bind(id)
            .bind(12345)
            .bind(login)
            .bind("Organization")
            .bind("all")
            .execute(&pool)
            .await?;
        }

        // Suspend one installation
        suspend_installation(2001, &pool).await?;

        // List all installations (should have 2)
        let all = list_all_installations(&pool).await?;
        assert_eq!(all.len(), 2);

        // List only active installations (should have 1)
        let active = list_active_installations(&pool).await?;
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].installation_id, 2002);

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_get_by_login(pool: SqlitePool) -> anyhow::Result<()> {
        // Manually insert a test installation
        sqlx::query(
            "INSERT INTO GitHubInstallations
             (installation_id, account_id, account_login, account_type, repository_selection)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(3001)
        .bind(12345)
        .bind("logintest")
        .bind("User")
        .bind("all")
        .execute(&pool)
        .await?;

        let retrieved = get_installation_by_login("logintest", &pool).await?;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().installation_id, 3001);

        Ok(())
    }
}
