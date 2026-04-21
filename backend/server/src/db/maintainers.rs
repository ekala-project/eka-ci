//! Database queries for attr path maintainers and maintainer requests

use anyhow::Result;
use sqlx::{Pool, Sqlite};

use crate::auth::types::{
    AttrPathMaintainerRequest, MaintainerDetail, MaintainerRequestDetail, RequestStatus,
};

// ============================================================================
// Maintainer Requests
// ============================================================================

/// Create a new maintainer request
pub async fn create_maintainer_request(
    attr_path: &str,
    github_user_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<i64> {
    let result = sqlx::query(
        r#"
        INSERT INTO AttrPathMaintainerRequests (attr_path, github_user_id, status)
        VALUES (?, ?, ?)
        "#,
    )
    .bind(attr_path)
    .bind(github_user_id)
    .bind(RequestStatus::Pending.as_str())
    .execute(pool)
    .await?;

    Ok(result.last_insert_rowid())
}

/// Get a maintainer request by ID
pub async fn get_maintainer_request(
    request_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<AttrPathMaintainerRequest>> {
    let request = sqlx::query_as(
        r#"
        SELECT id, attr_path, github_user_id, requested_at, status,
               reviewed_by_user_id, reviewed_at
        FROM AttrPathMaintainerRequests
        WHERE id = ?
        "#,
    )
    .bind(request_id)
    .fetch_optional(pool)
    .await?;

    Ok(request)
}

/// Get all pending maintainer requests
pub async fn list_pending_requests(pool: &Pool<Sqlite>) -> Result<Vec<MaintainerRequestDetail>> {
    let requests = sqlx::query_as(
        r#"
        SELECT
            r.id,
            r.attr_path,
            r.github_user_id,
            u.github_username,
            u.github_avatar_url,
            r.requested_at,
            r.status,
            r.reviewed_by_user_id,
            reviewer.github_username as reviewed_by_username,
            r.reviewed_at
        FROM AttrPathMaintainerRequests r
        INNER JOIN AuthenticatedUsers u ON r.github_user_id = u.github_id
        LEFT JOIN AuthenticatedUsers reviewer ON r.reviewed_by_user_id = reviewer.github_id
        WHERE r.status = 'pending'
        ORDER BY r.requested_at DESC
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(requests)
}

/// Get all requests for a specific user
pub async fn list_user_requests(
    github_user_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<Vec<MaintainerRequestDetail>> {
    let requests = sqlx::query_as(
        r#"
        SELECT
            r.id,
            r.attr_path,
            r.github_user_id,
            u.github_username,
            u.github_avatar_url,
            r.requested_at,
            r.status,
            r.reviewed_by_user_id,
            reviewer.github_username as reviewed_by_username,
            r.reviewed_at
        FROM AttrPathMaintainerRequests r
        INNER JOIN AuthenticatedUsers u ON r.github_user_id = u.github_id
        LEFT JOIN AuthenticatedUsers reviewer ON r.reviewed_by_user_id = reviewer.github_id
        WHERE r.github_user_id = ?
        ORDER BY r.requested_at DESC
        "#,
    )
    .bind(github_user_id)
    .fetch_all(pool)
    .await?;

    Ok(requests)
}

/// Check if a user already has a pending request for an attr path
pub async fn has_pending_request(
    attr_path: &str,
    github_user_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM AttrPathMaintainerRequests
        WHERE attr_path = ? AND github_user_id = ? AND status = 'pending'
        "#,
    )
    .bind(attr_path)
    .bind(github_user_id)
    .fetch_one(pool)
    .await?;

    Ok(count > 0)
}

/// Check if a user is already a maintainer for an attr path
pub async fn is_maintainer(
    attr_path: &str,
    github_user_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<bool> {
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM AttrPathMaintainers
        WHERE attr_path = ? AND github_user_id = ?
        "#,
    )
    .bind(attr_path)
    .bind(github_user_id)
    .fetch_one(pool)
    .await?;

    Ok(count > 0)
}

/// Approve a maintainer request and add the user as a maintainer
pub async fn approve_request(
    request_id: i64,
    reviewed_by_user_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let mut tx = pool.begin().await?;

    // Get the request details
    let request: Option<(String, i64)> = sqlx::query_as(
        "SELECT attr_path, github_user_id FROM AttrPathMaintainerRequests WHERE id = ?",
    )
    .bind(request_id)
    .fetch_optional(&mut *tx)
    .await?;

    let (attr_path, github_user_id) =
        request.ok_or_else(|| anyhow::anyhow!("Request not found"))?;

    // Update the request status
    sqlx::query(
        r#"
        UPDATE AttrPathMaintainerRequests
        SET status = 'approved', reviewed_by_user_id = ?, reviewed_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
    )
    .bind(reviewed_by_user_id)
    .bind(request_id)
    .execute(&mut *tx)
    .await?;

    // Add the user as a maintainer (ignore if already exists due to UNIQUE constraint)
    sqlx::query(
        r#"
        INSERT INTO AttrPathMaintainers (attr_path, github_user_id, added_by_user_id)
        VALUES (?, ?, ?)
        ON CONFLICT(attr_path, github_user_id) DO NOTHING
        "#,
    )
    .bind(&attr_path)
    .bind(github_user_id)
    .bind(reviewed_by_user_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(())
}

/// Reject a maintainer request
pub async fn reject_request(
    request_id: i64,
    reviewed_by_user_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE AttrPathMaintainerRequests
        SET status = 'rejected', reviewed_by_user_id = ?, reviewed_at = CURRENT_TIMESTAMP
        WHERE id = ?
        "#,
    )
    .bind(reviewed_by_user_id)
    .bind(request_id)
    .execute(pool)
    .await?;

    Ok(())
}

// ============================================================================
// Maintainers
// ============================================================================

/// Get all maintainers for an attr path with user details
pub async fn get_maintainers_for_attr_path(
    attr_path: &str,
    pool: &Pool<Sqlite>,
) -> Result<Vec<MaintainerDetail>> {
    let maintainers = sqlx::query_as(
        r#"
        SELECT
            m.attr_path,
            m.github_user_id,
            u.github_username,
            u.github_avatar_url,
            m.added_at,
            m.added_by_user_id,
            adder.github_username as added_by_username
        FROM AttrPathMaintainers m
        INNER JOIN AuthenticatedUsers u ON m.github_user_id = u.github_id
        LEFT JOIN AuthenticatedUsers adder ON m.added_by_user_id = adder.github_id
        WHERE m.attr_path = ?
        ORDER BY m.added_at ASC
        "#,
    )
    .bind(attr_path)
    .fetch_all(pool)
    .await?;

    Ok(maintainers)
}

/// Get maintainers for a job by job ID
/// Joins through Job -> name (attr_path) -> AttrPathMaintainers
pub async fn get_maintainers_for_job(
    job_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<Vec<MaintainerDetail>> {
    let maintainers = sqlx::query_as(
        r#"
        SELECT
            m.attr_path,
            m.github_user_id,
            u.github_username,
            u.github_avatar_url,
            m.added_at,
            m.added_by_user_id,
            adder.github_username as added_by_username
        FROM Job j
        INNER JOIN AttrPathMaintainers m ON j.name = m.attr_path
        INNER JOIN AuthenticatedUsers u ON m.github_user_id = u.github_id
        LEFT JOIN AuthenticatedUsers adder ON m.added_by_user_id = adder.github_id
        WHERE j.ROWID = ?
        ORDER BY m.added_at ASC
        "#,
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;

    Ok(maintainers)
}

/// Get the attr path (job name) for a job
pub async fn get_attr_path_for_job(job_id: i64, pool: &Pool<Sqlite>) -> Result<Option<String>> {
    let attr_path: Option<String> = sqlx::query_scalar("SELECT name FROM Job WHERE ROWID = ?")
        .bind(job_id)
        .fetch_optional(pool)
        .await?;

    Ok(attr_path)
}

/// Get all distinct attribute paths (job names) associated with the given
/// derivation path across all jobs that have built it.
///
/// Returns an empty Vec if the derivation is unknown or has no associated job
/// rows. Used to gate build-control operations on maintainer status.
pub async fn get_attr_paths_for_drv(
    drv_path: &str,
    pool: &Pool<Sqlite>,
) -> Result<Vec<String>> {
    let attr_paths: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT DISTINCT j.name
        FROM Job j
        INNER JOIN Drv d ON d.ROWID = j.drv_id
        WHERE d.drv_path = ?
        "#,
    )
    .bind(drv_path)
    .fetch_all(pool)
    .await?;

    Ok(attr_paths)
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_create_and_get_request(pool: SqlitePool) -> Result<()> {
        // Insert a test user first
        sqlx::query(
            "INSERT INTO AuthenticatedUsers (github_id, github_username, github_access_token) \
             VALUES (?, ?, ?)",
        )
        .bind(12345)
        .bind("testuser")
        .bind("test-token")
        .execute(&pool)
        .await?;

        // Create a request
        let request_id = create_maintainer_request("nixpkgs.python3", 12345, &pool).await?;

        // Get the request
        let request = get_maintainer_request(request_id, &pool).await?;
        assert!(request.is_some());
        let request = request.unwrap();
        assert_eq!(request.attr_path, "nixpkgs.python3");
        assert_eq!(request.github_user_id, 12345);
        assert_eq!(request.status, "pending");

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_approve_request(pool: SqlitePool) -> Result<()> {
        // Insert test users
        sqlx::query(
            "INSERT INTO AuthenticatedUsers (github_id, github_username, github_access_token) \
             VALUES (?, ?, ?)",
        )
        .bind(12345)
        .bind("requester")
        .bind("test-token")
        .execute(&pool)
        .await?;

        sqlx::query(
            "INSERT INTO AuthenticatedUsers (github_id, github_username, github_access_token) \
             VALUES (?, ?, ?)",
        )
        .bind(67890)
        .bind("admin")
        .bind("admin-token")
        .execute(&pool)
        .await?;

        // Create and approve a request
        let request_id = create_maintainer_request("nixpkgs.rust", 12345, &pool).await?;
        approve_request(request_id, 67890, &pool).await?;

        // Verify request is approved
        let request = get_maintainer_request(request_id, &pool).await?.unwrap();
        assert_eq!(request.status, "approved");
        assert_eq!(request.reviewed_by_user_id, Some(67890));

        // Verify maintainer was added
        let is_maintainer = is_maintainer("nixpkgs.rust", 12345, &pool).await?;
        assert!(is_maintainer);

        Ok(())
    }
}

/// Check if a GitHub user is a maintainer for ALL specified attr_paths
pub async fn is_maintainer_of_all_packages(
    github_user_id: i64,
    attr_paths: &[String],
    pool: &Pool<Sqlite>,
) -> Result<bool> {
    if attr_paths.is_empty() {
        return Ok(false);
    }

    // For each attr_path, check if the user is a maintainer
    for attr_path in attr_paths {
        let is_maint = is_maintainer(attr_path, github_user_id, pool).await?;
        if !is_maint {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Get GitHub user ID by GitHub username
pub async fn get_github_user_id_by_username(
    github_username: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<i64>> {
    let user_id: Option<i64> =
        sqlx::query_scalar("SELECT github_id FROM AuthenticatedUsers WHERE github_username = ?")
            .bind(github_username)
            .fetch_optional(pool)
            .await?;

    Ok(user_id)
}
