//! Handlers for maintainer requests (public endpoints for users)

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use sqlx::SqlitePool;

use super::github_api::GitHubApiClient;
use super::middleware::{AdminUser, AuthUser};
use super::types::{MaintainerDetail, MaintainerRequestDetail};
use crate::db::maintainers;

/// Shared state for request handlers that need GitHub API access
#[derive(Clone)]
pub struct RequestHandlerState {
    pub pool: SqlitePool,
    pub github_client: Arc<GitHubApiClient>,
}

/// Get all maintainers for a specific attr path (public endpoint)
pub async fn get_attr_path_maintainers(
    Path(attr_path): Path<String>,
    State(pool): State<SqlitePool>,
) -> Result<Json<Vec<MaintainerDetail>>, Response> {
    let maintainers = maintainers::get_maintainers_for_attr_path(&attr_path, &pool)
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

/// Get all maintainers for a job (public endpoint)
pub async fn get_job_maintainers(
    Path(job_id): Path<i64>,
    State(pool): State<SqlitePool>,
) -> Result<Json<Vec<MaintainerDetail>>, Response> {
    let maintainers = maintainers::get_maintainers_for_job(job_id, &pool)
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

/// Request to become a maintainer of an attr path (authenticated endpoint)
/// Auto-approves if user has triage+ access on the repository
pub async fn request_maintainer(
    user: AuthUser,
    Path(attr_path): Path<String>,
    State(state): State<RequestHandlerState>,
) -> Result<Json<serde_json::Value>, Response> {
    let pool = &state.pool;

    // Check if user is already a maintainer
    if maintainers::is_maintainer(&attr_path, user.github_id, pool)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to check maintainer status: {}", e) })),
            )
                .into_response()
        })?
    {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "You are already a maintainer of this attr path" })),
        )
            .into_response());
    }

    // Check if user already has a pending request
    if maintainers::has_pending_request(&attr_path, user.github_id, pool)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to check pending requests: {}", e) })),
            )
                .into_response()
        })?
    {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "You already have a pending request for this attr path" })),
        )
            .into_response());
    }

    // Try to determine the repository from the attr path or jobset
    // For now, we'll need to get this from a job that uses this attr path
    let repo_info = get_repo_for_attr_path(&attr_path, pool)
        .await
        .map_err(|e| {
            tracing::warn!(
                "Could not determine repository for attr path {}: {}",
                attr_path,
                e
            );
            None::<(String, String)>
        })
        .ok()
        .flatten();

    let mut auto_approved = false;

    // If we can determine the repo, check GitHub permissions
    if let Some((owner, repo_name)) = repo_info {
        // Get user's GitHub access token
        let access_token_result = sqlx::query_scalar::<_, String>(
            "SELECT github_access_token FROM AuthenticatedUsers WHERE github_id = ?",
        )
        .bind(user.github_id)
        .fetch_one(pool)
        .await;

        if let Ok(access_token) = access_token_result {
            // Check GitHub permissions
            match state
                .github_client
                .check_repo_permission(&access_token, &owner, &repo_name)
                .await
            {
                Ok(permission) if permission.is_triage_or_higher() => {
                    // User has triage+ access - auto-approve
                    tracing::info!(
                        "Auto-approving maintainer request for {} by user {} (has {:?} access)",
                        attr_path,
                        user.github_id,
                        permission
                    );

                    // Add directly as maintainer (self-added, so added_by_user_id is None)
                    sqlx::query(
                        r#"
                        INSERT INTO AttrPathMaintainers (attr_path, github_user_id, added_by_user_id)
                        VALUES (?, ?, ?)
                        "#,
                    )
                    .bind(&attr_path)
                    .bind(user.github_id)
                    .bind(Option::<i64>::None) // Self-added
                    .execute(pool)
                    .await
                    .map_err(|e| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({ "error": format!("Failed to add maintainer: {}", e) })),
                        )
                            .into_response()
                    })?;

                    auto_approved = true;
                },
                Ok(_) => {
                    tracing::info!(
                        "User {} does not have triage+ access to {}/{}, creating pending request",
                        user.github_id,
                        owner,
                        repo_name
                    );
                },
                Err(e) => {
                    tracing::warn!(
                        "Failed to check GitHub permissions for user {}: {}",
                        user.github_id,
                        e
                    );
                },
            }
        }
    }

    if !auto_approved {
        // Create a pending request
        let request_id = maintainers::create_maintainer_request(&attr_path, user.github_id, pool)
            .await
            .map_err(|e| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": format!("Failed to create request: {}", e) })),
                )
                    .into_response()
            })?;

        return Ok(Json(json!({
            "success": true,
            "status": "pending",
            "request_id": request_id,
            "message": "Your request has been submitted and is pending approval"
        })));
    }

    Ok(Json(json!({
        "success": true,
        "status": "approved",
        "message": "You have been added as a maintainer (auto-approved due to repository access)"
    })))
}

/// Helper function to get repository owner/name for an attr path
/// Looks up any job that uses this attr path and gets its repository
async fn get_repo_for_attr_path(
    attr_path: &str,
    pool: &SqlitePool,
) -> anyhow::Result<Option<(String, String)>> {
    let result: Option<(String, String)> = sqlx::query_as(
        r#"
        SELECT g.owner, g.repo_name
        FROM Job j
        INNER JOIN GitHubJobSets g ON j.jobset = g.ROWID
        WHERE j.name = ?
        LIMIT 1
        "#,
    )
    .bind(attr_path)
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

/// Get current user's maintainer requests (authenticated endpoint)
pub async fn get_my_requests(
    user: AuthUser,
    State(pool): State<SqlitePool>,
) -> Result<Json<Vec<MaintainerRequestDetail>>, Response> {
    let requests = maintainers::list_user_requests(user.github_id, &pool)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to fetch requests: {}", e) })),
            )
                .into_response()
        })?;

    Ok(Json(requests))
}

/// List all pending maintainer requests (admin only)
pub async fn list_pending_requests(
    _admin: AdminUser,
    State(pool): State<SqlitePool>,
) -> Result<Json<Vec<MaintainerRequestDetail>>, Response> {
    let requests = maintainers::list_pending_requests(&pool)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to fetch requests: {}", e) })),
            )
                .into_response()
        })?;

    Ok(Json(requests))
}

/// Approve a maintainer request (admin only)
pub async fn approve_request(
    admin: AdminUser,
    Path(request_id): Path<i64>,
    State(pool): State<SqlitePool>,
) -> Result<Json<serde_json::Value>, Response> {
    maintainers::approve_request(request_id, admin.github_id, &pool)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to approve request: {}", e) })),
            )
                .into_response()
        })?;

    Ok(Json(
        json!({ "success": true, "message": "Request approved and user added as maintainer" }),
    ))
}

/// Reject a maintainer request (admin only)
pub async fn reject_request(
    admin: AdminUser,
    Path(request_id): Path<i64>,
    State(pool): State<SqlitePool>,
) -> Result<Json<serde_json::Value>, Response> {
    maintainers::reject_request(request_id, admin.github_id, &pool)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to reject request: {}", e) })),
            )
                .into_response()
        })?;

    Ok(Json(
        json!({ "success": true, "message": "Request rejected" }),
    ))
}
