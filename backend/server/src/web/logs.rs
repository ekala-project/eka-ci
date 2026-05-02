// Log file handlers for build and hook logs

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Serialize;
use tracing::{error, warn};

use super::security::{parse_drv_id, path_is_under};
use super::state::AppState;

pub(super) async fn get_derivation_log(
    State(state): State<AppState>,
    Path(drv): Path<String>,
) -> impl IntoResponse {
    // Parse the drv parameter into a DrvId
    let drv_id = match parse_drv_id(&drv) {
        Ok(id) => id,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    // Construct the log file path: {logs_dir}/{drv_hash}/build.log
    let drv_hash = drv_id.drv_hash();
    let log_path = state.logs_dir.join(drv_hash).join("build.log");

    // Defense-in-depth: refuse to serve anything outside logs_dir.
    if !path_is_under(&state.logs_dir, &log_path) {
        error!(
            "Refusing to serve build log outside logs_dir: {}",
            log_path.display()
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "text/plain")],
            "internal error",
        )
            .into_response();
    }

    // Read the log file
    match tokio::fs::read_to_string(&log_path).await {
        Ok(contents) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            contents,
        )
            .into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            warn!(
                "Log file not found for {}: {}",
                drv_id.store_path(),
                log_path.display()
            );
            (
                StatusCode::NOT_FOUND,
                [(axum::http::header::CONTENT_TYPE, "text/plain")],
                format!(
                    "Build log not found for derivation: {}",
                    drv_id.store_path()
                ),
            )
                .into_response()
        },
        Err(e) => {
            error!("Failed to read log file for {}: {}", drv_id.store_path(), e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(axum::http::header::CONTENT_TYPE, "text/plain")],
                format!("Failed to read build log: {}", e),
            )
                .into_response()
        },
    }
}

pub(super) async fn get_hook_log(
    State(state): State<AppState>,
    Path((drv, hook_name)): Path<(String, String)>,
) -> impl IntoResponse {
    // Parse the drv parameter into a DrvId
    let drv_id = match parse_drv_id(&drv) {
        Ok(id) => id,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    // Construct the log file path: {logs_dir}/{drv_hash}/hook-{hook_name}.log
    let drv_hash = drv_id.drv_hash();
    let log_path = state
        .logs_dir
        .join(drv_hash)
        .join(format!("hook-{}.log", hook_name));

    // Defense-in-depth: refuse to serve anything outside logs_dir.
    if !path_is_under(&state.logs_dir, &log_path) {
        error!(
            "Refusing to serve hook log outside logs_dir: {}",
            log_path.display()
        );
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "text/plain")],
            "internal error",
        )
            .into_response();
    }

    // Read the log file
    match tokio::fs::read_to_string(&log_path).await {
        Ok(contents) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; charset=utf-8",
            )],
            contents,
        )
            .into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            warn!(
                "Hook log file not found for {} hook '{}': {}",
                drv_id.store_path(),
                hook_name,
                log_path.display()
            );
            (
                StatusCode::NOT_FOUND,
                [(axum::http::header::CONTENT_TYPE, "text/plain")],
                format!(
                    "Hook log not found for derivation: {} hook: {}",
                    drv_id.store_path(),
                    hook_name
                ),
            )
                .into_response()
        },
        Err(e) => {
            error!(
                "Failed to read hook log file for {} hook '{}': {}",
                drv_id.store_path(),
                hook_name,
                e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(axum::http::header::CONTENT_TYPE, "text/plain")],
                format!("Failed to read hook log: {}", e),
            )
                .into_response()
        },
    }
}

pub(super) async fn get_drv_hooks_handler(
    State(state): State<AppState>,
    Path(drv): Path<String>,
) -> impl IntoResponse {
    #[derive(Serialize)]
    struct HookExecutionResponse {
        id: i64,
        hook_name: String,
        started_at: String,
        completed_at: Option<String>,
        exit_code: Option<i32>,
        success: bool,
        log_path: String,
    }

    #[derive(Serialize)]
    struct HooksListResponse {
        drv_path: String,
        executions: Vec<HookExecutionResponse>,
        count: usize,
    }

    // Parse the drv parameter into a DrvId
    let drv_id = match parse_drv_id(&drv) {
        Ok(id) => id,
        Err((status, msg)) => {
            return (status, Json(serde_json::json!({ "error": msg }))).into_response();
        },
    };

    // Get hook executions from database
    match state
        .db_service
        .get_hook_executions_for_drv(&drv_id.store_path())
        .await
    {
        Ok(executions) => {
            let response = HooksListResponse {
                drv_path: drv_id.store_path().to_string(),
                count: executions.len(),
                executions: executions
                    .into_iter()
                    .map(|e| HookExecutionResponse {
                        id: e.id,
                        hook_name: e.hook_name,
                        started_at: e.started_at.to_rfc3339(),
                        completed_at: e.completed_at.map(|t| t.to_rfc3339()),
                        exit_code: e.exit_code,
                        success: e.success,
                        log_path: e.log_path,
                    })
                    .collect(),
            };
            (StatusCode::OK, Json(response)).into_response()
        },
        Err(e) => {
            error!(
                "Failed to get hook executions for {}: {}",
                drv_id.store_path(),
                e
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": "Failed to retrieve hook executions"
                })),
            )
                .into_response()
        },
    }
}
