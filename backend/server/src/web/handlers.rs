// Miscellaneous handlers (auth, metrics, admin, user profile)

use axum::extract::{Json, Path, State};
use axum::response::IntoResponse;
use prometheus::{Encoder, TextEncoder};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use super::state::AppState;
use crate::auth::{AdminUser, AuthUser};

// Auth handlers
pub(super) async fn auth_login_handler(State(state): State<AppState>) -> impl IntoResponse {
    crate::auth::handle_login(State(state.oauth_state())).await
}

pub(super) async fn auth_callback_handler(
    query: axum::extract::Query<crate::auth::oauth::CallbackParams>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::handle_callback(query, State(state.oauth_state())).await
}

pub(super) async fn auth_me_handler(
    user: AuthUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::handle_me(user, State(state.oauth_state())).await
}

pub(super) async fn metrics_handler(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = state.metrics_registry.gather();
    let mut buffer = Vec::new();

    if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
        error!("Failed to encode metrics: {}", e);
        return (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to encode metrics".to_string(),
        );
    }

    match String::from_utf8(buffer) {
        Ok(metrics_text) => (axum::http::StatusCode::OK, metrics_text),
        Err(e) => {
            error!("Failed to convert metrics to UTF-8: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to convert metrics to UTF-8".to_string(),
            )
        },
    }
}

// Approved users handlers
#[derive(Serialize, Deserialize)]
struct AddApprovedUserRequest {
    username: String,
    user_id: i64,
    notes: Option<String>,
}

#[derive(Serialize)]
struct ApprovedUserResponse {
    github_username: String,
    github_id: i64,
    approved_at: String,
    notes: Option<String>,
}

impl From<crate::db::ApprovedUser> for ApprovedUserResponse {
    fn from(user: crate::db::ApprovedUser) -> Self {
        Self {
            github_username: user.github_username,
            github_id: user.github_id,
            approved_at: user.approved_at,
            notes: user.notes,
        }
    }
}

pub(super) async fn list_approved_users_handler(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<ApprovedUserResponse>>, (axum::http::StatusCode, String)> {
    match state.db_service.list_approved_users().await {
        Ok(users) => Ok(Json(users.into_iter().map(Into::into).collect())),
        Err(e) => {
            error!("Failed to list approved users: {}", e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to list approved users: {}", e),
            ))
        },
    }
}

pub(super) async fn add_approved_user_handler(
    _admin: AdminUser,
    State(state): State<AppState>,
    Json(request): Json<AddApprovedUserRequest>,
) -> Result<Json<&'static str>, (axum::http::StatusCode, String)> {
    match state
        .db_service
        .add_approved_user(&request.username, request.user_id, request.notes.as_deref())
        .await
    {
        Ok(_) => Ok(Json("User approved successfully")),
        Err(e) => {
            error!("Failed to add approved user: {}", e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to add approved user: {}", e),
            ))
        },
    }
}

pub(super) async fn remove_approved_user_handler(
    _admin: AdminUser,
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> Result<Json<&'static str>, (axum::http::StatusCode, String)> {
    match state.db_service.remove_approved_user(&username).await {
        Ok(_) => Ok(Json("User removed successfully")),
        Err(e) => {
            error!("Failed to remove approved user: {}", e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to remove approved user: {}", e),
            ))
        },
    }
}

// User profile handlers
pub(super) async fn user_profile_handler(
    auth: AuthUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::profile::get_profile(auth, State(state.db_service.pool.clone())).await
}

pub(super) async fn update_user_profile_handler(
    auth: AuthUser,
    State(state): State<AppState>,
    body: Json<crate::auth::UpdateProfileRequest>,
) -> impl IntoResponse {
    crate::auth::profile::update_profile(auth, State(state.db_service.pool.clone()), body).await
}

pub(super) async fn user_maintained_paths_handler(
    auth: AuthUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::profile::get_maintained_paths(auth, State(state.db_service.pool.clone())).await
}

// Admin user management handlers
pub(super) async fn admin_list_users_handler(
    admin: AdminUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::admin::list_users(admin, State(state.db_service.pool.clone())).await
}

pub(super) async fn admin_promote_user_handler(
    admin: AdminUser,
    Path(github_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::admin::promote_user(admin, Path(github_id), State(state.db_service.pool.clone()))
        .await
}

pub(super) async fn admin_demote_user_handler(
    admin: AdminUser,
    Path(github_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::admin::demote_user(admin, Path(github_id), State(state.db_service.pool.clone()))
        .await
}

pub(super) async fn admin_delete_user_handler(
    admin: AdminUser,
    Path(github_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::admin::delete_user(admin, Path(github_id), State(state.db_service.pool.clone()))
        .await
}

pub(super) async fn admin_user_maintained_paths_handler(
    admin: AdminUser,
    Path(github_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::admin::get_user_maintained_paths(
        admin,
        Path(github_id),
        State(state.db_service.pool.clone()),
    )
    .await
}

// Admin attr path maintainer handlers
pub(super) async fn admin_add_maintainer_handler(
    admin: AdminUser,
    Path(attr_path): Path<String>,
    State(state): State<AppState>,
    body: Json<crate::auth::AddMaintainerRequest>,
) -> impl IntoResponse {
    crate::auth::admin::add_maintainer(
        admin,
        Path(attr_path),
        State(state.db_service.pool.clone()),
        body,
    )
    .await
}

pub(super) async fn admin_remove_maintainer_handler(
    admin: AdminUser,
    Path((attr_path, github_id)): Path<(String, i64)>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::admin::remove_maintainer(
        admin,
        Path((attr_path, github_id)),
        State(state.db_service.pool.clone()),
    )
    .await
}

pub(super) async fn admin_list_maintainers_handler(
    admin: AdminUser,
    Path(attr_path): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::admin::list_maintainers(
        admin,
        Path(attr_path),
        State(state.db_service.pool.clone()),
    )
    .await
}

// Admin cache management handlers
#[derive(Deserialize)]
struct InvalidateGitHubApiCacheRequest {
    github_id: i64,
    owner: String,
    repo: String,
}

pub(super) async fn admin_github_api_cache_clear_handler(
    State(state): State<AppState>,
    admin: AdminUser,
) -> impl IntoResponse {
    state.github_client.clear_cache().await;
    info!(
        "GitHub API permission cache cleared by admin github_id={}",
        admin.github_id
    );
    (
        axum::http::StatusCode::ACCEPTED,
        "GitHub API permission cache cleared",
    )
        .into_response()
}

pub(super) async fn admin_github_api_cache_invalidate_handler(
    State(state): State<AppState>,
    admin: AdminUser,
    Json(body): Json<InvalidateGitHubApiCacheRequest>,
) -> impl IntoResponse {
    let token_result = sqlx::query_scalar::<_, String>(
        "SELECT github_access_token FROM AuthenticatedUsers WHERE github_id = ?",
    )
    .bind(body.github_id)
    .fetch_optional(&state.db_service.pool)
    .await;

    let access_token = match token_result {
        Ok(Some(t)) => t,
        Ok(None) => {
            return (
                axum::http::StatusCode::NOT_FOUND,
                format!("No authenticated user with github_id={}", body.github_id),
            )
                .into_response();
        },
        Err(e) => {
            error!(
                "Failed to look up access token for github_id={}: {}",
                body.github_id, e
            );
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to look up target user's access token".to_string(),
            )
                .into_response();
        },
    };

    state
        .github_client
        .invalidate_cache(&access_token, &body.owner, &body.repo)
        .await;

    info!(
        "GitHub API permission cache invalidated for github_id={} {}/{} by admin github_id={}",
        body.github_id, body.owner, body.repo, admin.github_id
    );

    (
        axum::http::StatusCode::ACCEPTED,
        "GitHub API permission cache entry invalidated",
    )
        .into_response()
}
