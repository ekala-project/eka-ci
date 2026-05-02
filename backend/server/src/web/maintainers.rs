// Maintainer and build control handlers

use axum::extract::{Json, Path, State};
use axum::response::IntoResponse;
use tracing::{error, info};

use super::responses::{forbidden, internal_error, not_found, service_unavailable};
use super::security::parse_drv_id;
use super::state::AppState;
use crate::auth::{AdminUser, AuthUser};
use crate::scheduler::IngressTask;

pub(super) async fn get_attr_path_maintainers_handler(
    Path(attr_path): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::get_attr_path_maintainers(
        Path(attr_path),
        State(state.db_service.pool.clone()),
    )
    .await
}

pub(super) async fn get_job_maintainers_handler(
    Path(job_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::get_job_maintainers(Path(job_id), State(state.db_service.pool.clone()))
        .await
}

pub(super) async fn request_maintainer_handler(
    user: AuthUser,
    Path(attr_path): Path<String>,
    State(state): State<AppState>,
    body: Option<Json<crate::auth::RequestMaintainerRequest>>,
) -> impl IntoResponse {
    let request_state = crate::auth::RequestHandlerState {
        pool: state.db_service.pool.clone(),
        github_client: state.github_client.clone(),
    };

    crate::auth::requests::request_maintainer(user, Path(attr_path), State(request_state), body)
        .await
}

pub(super) async fn request_maintainer_for_job_handler(
    user: AuthUser,
    Path(job_id): Path<i64>,
    State(state): State<AppState>,
    body: Option<Json<crate::auth::RequestMaintainerRequest>>,
) -> impl IntoResponse {
    let request_state = crate::auth::RequestHandlerState {
        pool: state.db_service.pool.clone(),
        github_client: state.github_client.clone(),
    };

    crate::auth::requests::request_maintainer_for_job(
        user,
        Path(job_id),
        State(request_state),
        body,
    )
    .await
}

pub(super) async fn get_my_requests_handler(
    user: AuthUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::get_my_requests(user, State(state.db_service.pool.clone())).await
}

pub(super) async fn get_maintainer_request_handler(
    user: AuthUser,
    Path(request_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::get_request_by_id(
        user,
        Path(request_id),
        State(state.db_service.pool.clone()),
    )
    .await
}

pub(super) async fn admin_add_maintainer_by_username_handler(
    admin: AdminUser,
    Path(attr_path): Path<String>,
    State(state): State<AppState>,
    body: Json<crate::auth::requests::AddMaintainerByUsernameRequest>,
) -> impl IntoResponse {
    crate::auth::requests::add_maintainer_by_username(
        admin,
        Path(attr_path),
        State(state.db_service.pool.clone()),
        body,
    )
    .await
}

pub(super) async fn admin_list_pending_requests_handler(
    admin: AdminUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::list_pending_requests(admin, State(state.db_service.pool.clone())).await
}

pub(super) async fn admin_approve_request_handler(
    admin: AdminUser,
    Path(request_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::approve_request(
        admin,
        Path(request_id),
        State(state.db_service.pool.clone()),
    )
    .await
}

pub(super) async fn admin_reject_request_handler(
    admin: AdminUser,
    Path(request_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::reject_request(
        admin,
        Path(request_id),
        State(state.db_service.pool.clone()),
    )
    .await
}

/// Ensure the authenticated user can modify builds for the given derivation:
/// either an admin, or a maintainer of every attr path the derivation is
/// associated with via `Job` rows.
async fn ensure_drv_maintainer_access(
    auth_user: &AuthUser,
    drv_path: &str,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    action_verb_phrase: &str,
) -> Result<Vec<String>, axum::response::Response> {
    if auth_user.is_admin() {
        return Ok(Vec::new());
    }

    let attr_paths = crate::db::maintainers::get_attr_paths_for_drv(drv_path, pool)
        .await
        .map_err(|e| {
            error!("Failed to get attr paths for drv {}: {}", drv_path, e);
            internal_error(format!("Failed to look up derivation: {}", e))
        })?;

    if attr_paths.is_empty() {
        return Err(forbidden(format!(
            "No attribute paths are associated with this derivation; only admins can {}",
            action_verb_phrase
        )));
    }

    for attr_path in &attr_paths {
        let allowed = auth_user
            .can_modify_build(pool, attr_path)
            .await
            .map_err(|e| {
                error!("Failed to check maintainer status: {}", e);
                internal_error(format!("Failed to check maintainer status: {}", e))
            })?;
        if !allowed {
            return Err(forbidden(format!(
                "You must be a maintainer of all attribute paths for this derivation to {}",
                action_verb_phrase
            )));
        }
    }

    Ok(attr_paths)
}

pub(super) async fn rebuild_drv_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(drv): Path<String>,
) -> impl IntoResponse {
    let drv_id = match parse_drv_id(&drv) {
        Ok(id) => id,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    match state.db_service.get_drv(&drv_id).await {
        Ok(Some(_)) => {},
        Ok(None) => return not_found("Derivation not found"),
        Err(e) => {
            error!("Failed to look up drv {}: {}", drv_id.store_path(), e);
            return internal_error(format!("Failed to look up derivation: {}", e));
        },
    }

    if let Err(resp) = ensure_drv_maintainer_access(
        &auth_user,
        &drv_id.store_path(),
        &state.db_service.pool,
        "rebuild this derivation",
    )
    .await
    {
        return resp;
    }

    let sender = match state.ingress_sender.as_ref() {
        Some(s) => s,
        None => return service_unavailable("Scheduler is not available"),
    };

    if let Err(e) = sender
        .send(IngressTask::RebuildFailed(std::sync::Arc::new(
            drv_id.clone(),
        )))
        .await
    {
        error!(
            "Failed to enqueue rebuild for {}: {}",
            drv_id.store_path(),
            e
        );
        return service_unavailable("Failed to enqueue rebuild");
    }

    info!(
        "Rebuild requested for {} by user {} (github_id={})",
        drv_id.store_path(),
        auth_user.claims.username,
        auth_user.github_id,
    );
    (axum::http::StatusCode::ACCEPTED, "Rebuild queued").into_response()
}

pub(super) async fn admin_rebuild_all_failed_handler(
    State(state): State<AppState>,
    admin: AdminUser,
) -> impl IntoResponse {
    let sender = match state.ingress_sender.as_ref() {
        Some(s) => s,
        None => return service_unavailable("Scheduler is not available"),
    };

    if let Err(e) = sender.send(IngressTask::RebuildAllFailed).await {
        error!("Failed to enqueue rebuild-all-failed: {}", e);
        return service_unavailable("Failed to enqueue rebuild");
    }

    info!(
        "RebuildAllFailed requested by admin github_id={}",
        admin.github_id
    );
    (
        axum::http::StatusCode::ACCEPTED,
        "All failed derivations queued for rebuild",
    )
        .into_response()
}
