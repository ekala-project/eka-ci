use std::net::{SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Json, Path, State, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use octocrab::models::webhook_events::WebhookEventPayload as WEP;
use prometheus::{Encoder, Registry, TextEncoder};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::auth::{AdminUser, AuthUser, JwtService, OAuthConfig};
use crate::db::github::CheckRun;
use crate::git::GitTask;
use crate::github::GitHubTask;

#[derive(Clone)]
struct AppState {
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    octocrab: Option<octocrab::Octocrab>,
    metrics_registry: Arc<Registry>,
    require_approval: bool,
    db_service: crate::db::DbService,
    jwt_service: JwtService,
    oauth_config: OAuthConfig,
    logs_dir: PathBuf,
    websocket_service: crate::services::WebSocketService,
}

// Implement FromRef so extractors can access JwtService from AppState
impl axum::extract::FromRef<AppState> for JwtService {
    fn from_ref(state: &AppState) -> Self {
        state.jwt_service.clone()
    }
}

pub struct WebService {
    listener: TcpListener,
    state: AppState,
}

impl WebService {
    pub async fn bind_to_address(
        socket: &SocketAddrV4,
        git_sender: mpsc::Sender<GitTask>,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        octocrab: Option<octocrab::Octocrab>,
        metrics_registry: Arc<Registry>,
        require_approval: bool,
        db_service: crate::db::DbService,
        jwt_service: JwtService,
        oauth_config: OAuthConfig,
        logs_dir: PathBuf,
        websocket_service: crate::services::WebSocketService,
    ) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(socket)
            .await
            .context(format!("failed to bind to tcp socket at {socket}"))?;

        Ok(Self {
            listener,
            state: AppState {
                git_sender,
                github_sender,
                octocrab,
                metrics_registry,
                require_approval,
                db_service,
                jwt_service,
                oauth_config,
                logs_dir,
                websocket_service,
            },
        })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        // If the call fails either the system ran out of resources or libc is broken, for both of
        // these cases a panic seems appropiate.
        self.listener
            .local_addr()
            .expect("getsockname should always succeed on a properly initialized listener")
    }

    pub async fn run(self, cancellation_token: CancellationToken) {
        let app = Router::new()
            .nest("/v1", api_routes())
            .nest("/github", github_routes())
            .with_state(self.state);

        if let Err(e) = axum::serve(self.listener, app)
            .with_graceful_shutdown(async move {
                cancellation_token.cancelled().await;
                info!("Web service shutting down")
            })
            .await
        {
            error!(error = %e, "Failed to start web service");
            return;
        }

        info!("Web service shutdown gracefully")
    }
}

/// Prefixed with /github/ path
fn github_routes() -> Router<AppState> {
    Router::new()
        // Public routes
        .route("/webhook", post(handle_github_webhook))
        // Auth routes
        .route("/auth/login", get(auth_login_handler))
        .route("/auth/callback", get(auth_callback_handler))
        .route("/auth/me", get(auth_me_handler))
}

/// Prefixed with /v1 path
fn api_routes() -> Router<AppState> {
    Router::new()
        // Public routes
        .route("/logs/{drv}", get(get_derivation_log))
        .route("/metrics", get(metrics_handler))
        .route("/commits/{sha}/check_runs", get(get_check_runs_for_commit))
        // WebSocket route for real-time updates
        .route("/ws/builds", get(websocket_handler))
        // Repository management routes
        .route("/repositories", get(list_repositories_handler))
        .route("/repositories/{owner}/{repo}", get(get_repository_handler))
        .route("/repositories/{owner}/{repo}/commits", get(list_repository_commits_handler))
        // Job and build status routes
        .route("/commits/{sha}/jobs", get(get_commit_jobs_handler))
        .route("/jobs/{jobset_id}", get(get_jobset_details_handler))
        .route("/jobs/{jobset_id}/drvs", get(get_jobset_drvs_handler))
        // Derivation details routes
        .route("/drvs/{drv}", get(get_drv_details_handler))
        .route("/drvs/{drv}/dependencies", get(get_drv_dependencies_handler))
        // Admin routes (protected)
        .route("/admin/approved-users", get(list_approved_users_handler))
        .route("/admin/approved-users", post(add_approved_user_handler))
        .route(
            "/admin/approved-users/{username}",
            axum::routing::delete(remove_approved_user_handler),
        )
}

async fn handle_github_webhook(State(state): State<AppState>, body: axum::body::Bytes) {
    use octocrab::models::webhook_events::EventInstallation;
    use serde_json::Value;
    use tracing::warn;

    use crate::github::handle_webhook_payload;

    // First deserialize as generic JSON to extract top-level fields
    let webhook_json: Value = match serde_json::from_slice(&body) {
        Ok(json) => json,
        Err(e) => {
            warn!("Failed to parse webhook JSON: {:?}", e);
            return;
        },
    };

    // Extract repository info from top level
    let repository_info = webhook_json.get("repository").and_then(|repo| {
        let owner = repo.get("owner")?.get("login")?.as_str()?;
        let name = repo.get("name")?.as_str()?;
        Some((owner.to_string(), name.to_string()))
    });

    // Extract installation from top level if present
    let installation: Option<EventInstallation> = webhook_json
        .get("installation")
        .and_then(|inst| serde_json::from_value(inst.clone()).ok());

    // Deserialize the specific payload
    let webhook_payload: WEP = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(e) => {
            warn!("Failed to deserialize webhook payload: {:?}", e);
            return;
        },
    };

    handle_webhook_payload(
        webhook_payload,
        repository_info,
        installation,
        state.git_sender,
        state.github_sender,
        state.octocrab,
        state.require_approval,
        state.db_service,
    )
    .await;
}

async fn get_derivation_log(
    State(state): State<AppState>,
    axum::extract::Path(drv): axum::extract::Path<String>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    use tracing::warn;

    use crate::db::model::drv_id::DrvId;

    // Parse the drv parameter into a DrvId
    let drv_id = match DrvId::try_from(drv.as_str()) {
        Ok(id) => id,
        Err(e) => {
            warn!("Invalid drv format: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                [(axum::http::header::CONTENT_TYPE, "text/plain")],
                format!("Invalid derivation format: {}", e),
            )
                .into_response();
        },
    };

    // Construct the log file path: {logs_dir}/{drv_hash}/build.log
    let drv_hash = drv_id.drv_hash();
    let log_path = state.logs_dir.join(drv_hash).join("build.log");

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

// Auth handlers
async fn auth_login_handler(State(state): State<AppState>) -> impl IntoResponse {
    let oauth_state = crate::auth::oauth::AppState {
        db: state.db_service.pool.clone(),
        jwt_service: state.jwt_service.clone(),
        oauth_config: state.oauth_config.clone(),
    };
    crate::auth::handle_login(State(oauth_state)).await
}

async fn auth_callback_handler(
    query: axum::extract::Query<crate::auth::oauth::CallbackParams>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let oauth_state = crate::auth::oauth::AppState {
        db: state.db_service.pool.clone(),
        jwt_service: state.jwt_service.clone(),
        oauth_config: state.oauth_config.clone(),
    };
    crate::auth::handle_callback(query, State(oauth_state)).await
}

async fn auth_me_handler(user: AuthUser, State(state): State<AppState>) -> impl IntoResponse {
    let oauth_state = crate::auth::oauth::AppState {
        db: state.db_service.pool.clone(),
        jwt_service: state.jwt_service.clone(),
        oauth_config: state.oauth_config.clone(),
    };
    crate::auth::handle_me(user, State(oauth_state)).await
}

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
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

use serde::{Deserialize, Serialize};

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

async fn list_approved_users_handler(
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

async fn add_approved_user_handler(
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

async fn remove_approved_user_handler(
    _admin: AdminUser,
    State(state): State<AppState>,
    axum::extract::Path(username): axum::extract::Path<String>,
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

async fn get_check_runs_for_commit(
    State(state): State<AppState>,
    Path(sha): Path<String>,
) -> Json<Vec<CheckRun>> {
    match state.db_service.check_runs_for_commit(&sha).await {
        Ok(check_runs) => Json(check_runs),
        Err(e) => {
            error!("Failed to fetch check_runs for commit {}: {}", sha, e);
            Json(vec![])
        },
    }
}

// Repository management handlers
async fn list_repositories_handler(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::db::github::RepositoryInfo>>, (axum::http::StatusCode, String)> {
    match state.db_service.list_repositories().await {
        Ok(repos) => Ok(Json(repos)),
        Err(e) => {
            error!("Failed to list repositories: {}", e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to list repositories: {}", e),
            ))
        },
    }
}

async fn get_repository_handler(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
) -> Result<Json<crate::db::github::RepositoryInfo>, (axum::http::StatusCode, String)> {
    match state.db_service.get_repository(&owner, &repo).await {
        Ok(Some(repo_info)) => Ok(Json(repo_info)),
        Ok(None) => Err((
            axum::http::StatusCode::NOT_FOUND,
            format!("Repository {}/{} not found", owner, repo),
        )),
        Err(e) => {
            error!("Failed to get repository {}/{}: {}", owner, repo, e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get repository: {}", e),
            ))
        },
    }
}

#[derive(Deserialize)]
struct CommitsQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    20
}

async fn list_repository_commits_handler(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    axum::extract::Query(query): axum::extract::Query<CommitsQuery>,
) -> Result<Json<Vec<crate::db::github::CommitInfo>>, (axum::http::StatusCode, String)> {
    match state.db_service.list_repository_commits(&owner, &repo, query.limit).await {
        Ok(commits) => Ok(Json(commits)),
        Err(e) => {
            error!("Failed to list commits for {}/{}: {}", owner, repo, e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to list commits: {}", e),
            ))
        },
    }
}

// Job and build status handlers
async fn get_commit_jobs_handler(
    State(state): State<AppState>,
    Path(sha): Path<String>,
) -> Result<Json<Vec<crate::db::github::CommitJob>>, (axum::http::StatusCode, String)> {
    match state.db_service.get_commit_jobs(&sha).await {
        Ok(jobs) => Ok(Json(jobs)),
        Err(e) => {
            error!("Failed to get jobs for commit {}: {}", sha, e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get commit jobs: {}", e),
            ))
        },
    }
}

async fn get_jobset_details_handler(
    State(state): State<AppState>,
    Path(jobset_id): Path<i64>,
) -> Result<Json<crate::db::github::JobSetDetails>, (axum::http::StatusCode, String)> {
    match state.db_service.get_jobset_details(jobset_id).await {
        Ok(details) => Ok(Json(details)),
        Err(e) => {
            error!("Failed to get jobset details for {}: {}", jobset_id, e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get jobset details: {}", e),
            ))
        },
    }
}

#[derive(Deserialize)]
struct DrvsQuery {
    #[serde(default = "default_drv_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_drv_limit() -> i64 {
    100
}

async fn get_jobset_drvs_handler(
    State(state): State<AppState>,
    Path(jobset_id): Path<i64>,
    axum::extract::Query(query): axum::extract::Query<DrvsQuery>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    // TODO: Add state filter support when DrvBuildState implements FromStr
    let state_filter = None;

    match state.db_service.get_jobset_drvs(jobset_id, state_filter, query.limit, query.offset).await {
        Ok(drvs) => {
            // Also get total count
            match state.db_service.count_jobset_drvs(jobset_id).await {
                Ok(total) => Ok(Json(serde_json::json!({
                    "total": total,
                    "drvs": drvs,
                }))),
                Err(e) => {
                    error!("Failed to count drvs for jobset {}: {}", jobset_id, e);
                    Ok(Json(serde_json::json!({
                        "drvs": drvs,
                    })))
                },
            }
        },
        Err(e) => {
            error!("Failed to get drvs for jobset {}: {}", jobset_id, e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get jobset drvs: {}", e),
            ))
        },
    }
}

// Derivation details handlers
async fn get_drv_details_handler(
    State(state): State<AppState>,
    axum::extract::Path(drv): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    use crate::db::model::drv_id::DrvId;

    // Parse the drv parameter into a DrvId
    let drv_id = match DrvId::try_from(drv.as_str()) {
        Ok(id) => id,
        Err(e) => {
            return Err((
                axum::http::StatusCode::BAD_REQUEST,
                format!("Invalid derivation format: {}", e),
            ))
        },
    };

    match state.db_service.get_drv_details(&drv_id).await {
        Ok(Some(details)) => {
            // Also get dependency count
            match state.db_service.count_drv_dependencies(&drv_id).await {
                Ok(dep_count) => Ok(Json(serde_json::json!({
                    "drv_path": details.drv_path,
                    "system": details.system,
                    "build_state": details.build_state,
                    "is_fod": details.is_fod,
                    "required_system_features": details.required_system_features,
                    "dependency_count": dep_count,
                }))),
                Err(e) => {
                    error!("Failed to count dependencies for {}: {}", drv, e);
                    Ok(Json(serde_json::to_value(details).unwrap()))
                },
            }
        },
        Ok(None) => Err((
            axum::http::StatusCode::NOT_FOUND,
            format!("Derivation not found: {}", drv),
        )),
        Err(e) => {
            error!("Failed to get drv details for {}: {}", drv, e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get derivation details: {}", e),
            ))
        },
    }
}

async fn get_drv_dependencies_handler(
    State(state): State<AppState>,
    axum::extract::Path(drv): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    use crate::db::model::drv_id::DrvId;

    // Parse the drv parameter into a DrvId
    let drv_id = match DrvId::try_from(drv.as_str()) {
        Ok(id) => id,
        Err(e) => {
            return Err((
                axum::http::StatusCode::BAD_REQUEST,
                format!("Invalid derivation format: {}", e),
            ))
        },
    };

    match state.db_service.get_drv_dependencies(&drv_id).await {
        Ok(deps) => {
            let count = deps.len() as i64;
            Ok(Json(serde_json::json!({
                "drv_path": drv,
                "dependencies": deps,
                "dependency_count": count,
            })))
        },
        Err(e) => {
            error!("Failed to get dependencies for {}: {}", drv, e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get derivation dependencies: {}", e),
            ))
        },
    }
}

/// WebSocket handler for real-time build updates
async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| async move {
        state.websocket_service.handle_connection(socket).await
    })
}
