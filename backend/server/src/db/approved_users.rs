use anyhow::Result;
use sqlx::{FromRow, Pool, Sqlite};

#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct ApprovedUser {
    pub github_username: String,
    pub github_id: i64,
    pub approved_at: String, // SQLite stores as TEXT
    pub notes: Option<String>,
}

/// Check if a GitHub user is approved to create jobsets
pub async fn is_user_approved(username: &str, user_id: i64, pool: &Pool<Sqlite>) -> Result<bool> {
    // Check by both username and user_id for flexibility
    // User ID is more reliable since usernames can change
    let result: Option<i64> = sqlx::query_scalar(
        "SELECT ROWID FROM ApprovedUsers WHERE github_username = ? OR github_id = ?",
    )
    .bind(username)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    Ok(result.is_some())
}

/// Add a user to the approved users list
pub async fn add_approved_user(
    username: &str,
    user_id: i64,
    notes: Option<&str>,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query("INSERT INTO ApprovedUsers (github_username, github_id, notes) VALUES (?, ?, ?)")
        .bind(username)
        .bind(user_id)
        .bind(notes)
        .execute(pool)
        .await?;

    Ok(())
}

/// Remove a user from the approved users list by username
pub async fn remove_approved_user(username: &str, pool: &Pool<Sqlite>) -> Result<()> {
    sqlx::query("DELETE FROM ApprovedUsers WHERE github_username = ?")
        .bind(username)
        .execute(pool)
        .await?;

    Ok(())
}

/// List all approved users
pub async fn list_approved_users(pool: &Pool<Sqlite>) -> Result<Vec<ApprovedUser>> {
    let users = sqlx::query_as(
        "SELECT github_username, github_id, approved_at, notes FROM ApprovedUsers ORDER BY \
         approved_at DESC",
    )
    .fetch_all(pool)
    .await?;

    Ok(users)
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_approved_users(pool: SqlitePool) -> anyhow::Result<()> {
        // Initially no users
        assert!(!is_user_approved("testuser", 12345, &pool).await?);

        // Add a user
        add_approved_user("testuser", 12345, Some("Test approval"), &pool).await?;

        // User should now be approved
        assert!(is_user_approved("testuser", 12345, &pool).await?);

        // Check by username only
        assert!(is_user_approved("testuser", 99999, &pool).await?);

        // Check by user_id only
        assert!(is_user_approved("wrongname", 12345, &pool).await?);

        // List users
        let users = list_approved_users(&pool).await?;
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].github_username, "testuser");
        assert_eq!(users[0].github_id, 12345);
        assert_eq!(users[0].notes, Some("Test approval".to_string()));

        // Remove user
        remove_approved_user("testuser", &pool).await?;

        // User should no longer be approved
        assert!(!is_user_approved("testuser", 12345, &pool).await?);

        Ok(())
    }
}
