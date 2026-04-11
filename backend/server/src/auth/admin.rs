use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use sqlx::SqlitePool;

use super::middleware::AdminUser;
use super::types::{AddMaintainerRequest, AttrPathMaintainer, UserDetail};

/// List all users (admin only)
pub async fn list_users(
    _admin: AdminUser,
    State(pool): State<SqlitePool>,
) -> Result<Json<Vec<UserDetail>>, Response> {
    let users = sqlx::query_as::<_, UserDetail>(
        r#"
        SELECT
            github_id,
            github_username,
            github_avatar_url,
            is_admin,
            created_at,
            last_login
        FROM AuthenticatedUsers
        ORDER BY created_at DESC
        "#,
    )
    .fetch_all(&pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to fetch users: {}", e) })),
        )
            .into_response()
    })?;

    Ok(Json(users))
}

/// Promote a user to admin (admin only)
pub async fn promote_user(
    _admin: AdminUser,
    Path(github_id): Path<i64>,
    State(pool): State<SqlitePool>,
) -> Result<Json<serde_json::Value>, Response> {
    let result = sqlx::query(
        r#"
        UPDATE AuthenticatedUsers
        SET is_admin = 1
        WHERE github_id = ?
        "#,
    )
    .bind(github_id)
    .execute(&pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to promote user: {}", e) })),
        )
            .into_response()
    })?;

    if result.rows_affected() == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "User not found" })),
        )
            .into_response());
    }

    Ok(Json(json!({ "success": true })))
}

/// Demote a user from admin (admin only)
pub async fn demote_user(
    _admin: AdminUser,
    Path(github_id): Path<i64>,
    State(pool): State<SqlitePool>,
) -> Result<Json<serde_json::Value>, Response> {
    let result = sqlx::query(
        r#"
        UPDATE AuthenticatedUsers
        SET is_admin = 0
        WHERE github_id = ?
        "#,
    )
    .bind(github_id)
    .execute(&pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to demote user: {}", e) })),
        )
            .into_response()
    })?;

    if result.rows_affected() == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "User not found" })),
        )
            .into_response());
    }

    Ok(Json(json!({ "success": true })))
}

/// Delete a user (admin only)
pub async fn delete_user(
    _admin: AdminUser,
    Path(github_id): Path<i64>,
    State(pool): State<SqlitePool>,
) -> Result<Json<serde_json::Value>, Response> {
    let result = sqlx::query(
        r#"
        DELETE FROM AuthenticatedUsers
        WHERE github_id = ?
        "#,
    )
    .bind(github_id)
    .execute(&pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to delete user: {}", e) })),
        )
            .into_response()
    })?;

    if result.rows_affected() == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "User not found" })),
        )
            .into_response());
    }

    Ok(Json(json!({ "success": true })))
}

/// Get attr paths maintained by a user (admin only)
pub async fn get_user_maintained_paths(
    _admin: AdminUser,
    Path(github_id): Path<i64>,
    State(pool): State<SqlitePool>,
) -> Result<Json<Vec<String>>, Response> {
    let paths = sqlx::query_scalar::<_, String>(
        r#"
        SELECT attr_path
        FROM AttrPathMaintainers
        WHERE github_user_id = ?
        ORDER BY attr_path
        "#,
    )
    .bind(github_id)
    .fetch_all(&pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to fetch maintained paths: {}", e) })),
        )
            .into_response()
    })?;

    Ok(Json(paths))
}

/// Add a maintainer to an attr path (admin only)
pub async fn add_maintainer(
    admin: AdminUser,
    Path(attr_path): Path<String>,
    State(pool): State<SqlitePool>,
    Json(req): Json<AddMaintainerRequest>,
) -> Result<Json<serde_json::Value>, Response> {
    // Verify the user exists
    let user_exists = sqlx::query_scalar::<_, bool>(
        r#"
        SELECT COUNT(*) > 0
        FROM AuthenticatedUsers
        WHERE github_id = ?
        "#,
    )
    .bind(req.github_user_id)
    .fetch_one(&pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to verify user: {}", e) })),
        )
            .into_response()
    })?;

    if !user_exists {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "User not found" })),
        )
            .into_response());
    }

    // Add the maintainer
    sqlx::query(
        r#"
        INSERT INTO AttrPathMaintainers (attr_path, github_user_id, added_by_user_id)
        VALUES (?, ?, ?)
        "#,
    )
    .bind(&attr_path)
    .bind(req.github_user_id)
    .bind(admin.github_id)
    .execute(&pool)
    .await
    .map_err(|e| {
        if e.to_string().contains("UNIQUE constraint failed") {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": "User is already a maintainer of this attr path" })),
            )
                .into_response();
        }
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to add maintainer: {}", e) })),
        )
            .into_response()
    })?;

    Ok(Json(json!({ "success": true })))
}

/// Remove a maintainer from an attr path (admin only)
pub async fn remove_maintainer(
    _admin: AdminUser,
    Path((attr_path, github_id)): Path<(String, i64)>,
    State(pool): State<SqlitePool>,
) -> Result<Json<serde_json::Value>, Response> {
    let result = sqlx::query(
        r#"
        DELETE FROM AttrPathMaintainers
        WHERE attr_path = ? AND github_user_id = ?
        "#,
    )
    .bind(&attr_path)
    .bind(github_id)
    .execute(&pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to remove maintainer: {}", e) })),
        )
            .into_response()
    })?;

    if result.rows_affected() == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "Maintainer relationship not found" })),
        )
            .into_response());
    }

    Ok(Json(json!({ "success": true })))
}

/// List maintainers for an attr path (admin only)
pub async fn list_maintainers(
    _admin: AdminUser,
    Path(attr_path): Path<String>,
    State(pool): State<SqlitePool>,
) -> Result<Json<Vec<AttrPathMaintainer>>, Response> {
    let maintainers = sqlx::query_as::<_, AttrPathMaintainer>(
        r#"
        SELECT ROWID, attr_path, github_user_id, added_at, added_by_user_id
        FROM AttrPathMaintainers
        WHERE attr_path = ?
        ORDER BY added_at
        "#,
    )
    .bind(&attr_path)
    .fetch_all(&pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to fetch maintainers: {}", e) })),
        )
            .into_response()
    })?;

    Ok(Json(maintainers))
}
