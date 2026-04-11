use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use sqlx::SqlitePool;

use super::middleware::AuthUser;
use super::types::{UpdateProfileRequest, UserInfo, UserProfile};

/// Get current user's profile
pub async fn get_profile(
    auth: AuthUser,
    State(pool): State<SqlitePool>,
) -> Result<Json<UserProfile>, Response> {
    // Get user info
    let user = sqlx::query_as::<
        _,
        (
            String,
            Option<String>,
            bool,
            chrono::NaiveDateTime,
            chrono::NaiveDateTime,
        ),
    >(
        r#"
        SELECT
            github_username,
            github_avatar_url,
            is_admin,
            created_at,
            last_login
        FROM AuthenticatedUsers
        WHERE github_id = ?
        "#,
    )
    .bind(auth.github_id)
    .fetch_one(&pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to fetch user: {}", e) })),
        )
            .into_response()
    })?;

    // Get maintained attr paths
    let maintained_paths = sqlx::query_scalar::<_, String>(
        r#"
        SELECT attr_path
        FROM AttrPathMaintainers
        WHERE github_user_id = ?
        ORDER BY attr_path
        "#,
    )
    .bind(auth.github_id)
    .fetch_all(&pool)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to fetch maintained paths: {}", e) })),
        )
            .into_response()
    })?;

    let profile = UserProfile {
        user: UserInfo {
            github_id: auth.github_id,
            username: user.0,
            avatar_url: user.1,
            is_admin: user.2,
        },
        maintained_paths,
        created_at: user.3,
        last_login: user.4,
    };

    Ok(Json(profile))
}

/// Update current user's profile
pub async fn update_profile(
    _auth: AuthUser,
    State(_pool): State<SqlitePool>,
    Json(_req): Json<UpdateProfileRequest>,
) -> Result<Json<serde_json::Value>, Response> {
    // Future: Add user preferences, notification settings, etc.
    // For now, just return success
    Ok(Json(json!({ "success": true })))
}

/// Get attr paths maintained by current user
pub async fn get_maintained_paths(
    auth: AuthUser,
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
    .bind(auth.github_id)
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
