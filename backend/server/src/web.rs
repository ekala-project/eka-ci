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
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};
use tracing::{error, info};

use crate::auth::{AdminUser, AuthUser, JwtService, OAuthConfig};
use crate::db::github::CheckRun;
use crate::git::GitTask;
use crate::github::GitHubTask;
use crate::webhook_security::{check_webhook_secret_configured, verify_webhook_signature};

#[derive(Clone)]
struct AppState {
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    octocrab: Option<octocrab::Octocrab>,
    metrics_registry: Arc<Registry>,
    require_approval: bool,
    merge_queue_require_approval: bool,
    db_service: crate::db::DbService,
    graph_handle: crate::graph::GraphServiceHandle,
    jwt_service: JwtService,
    oauth_config: OAuthConfig,
    logs_dir: PathBuf,
    websocket_service: crate::services::WebSocketService,
    github_app_configs: Arc<std::collections::HashMap<String, crate::config::GitHubAppConfig>>,
    webhook_secret: Option<String>,
    github_client: Arc<crate::auth::GitHubApiClient>,
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
        merge_queue_require_approval: bool,
        db_service: crate::db::DbService,
        graph_handle: crate::graph::GraphServiceHandle,
        jwt_service: JwtService,
        oauth_config: OAuthConfig,
        logs_dir: PathBuf,
        websocket_service: crate::services::WebSocketService,
        github_app_configs: Arc<std::collections::HashMap<String, crate::config::GitHubAppConfig>>,
        webhook_secret: Option<String>,
        github_client: Arc<crate::auth::GitHubApiClient>,
    ) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(socket)
            .await
            .context(format!("failed to bind to tcp socket at {socket}"))?;

        // Check and warn if webhook secret is not configured
        check_webhook_secret_configured(&webhook_secret);

        Ok(Self {
            listener,
            state: AppState {
                git_sender,
                github_sender,
                octocrab,
                metrics_registry,
                require_approval,
                merge_queue_require_approval,
                db_service,
                graph_handle,
                jwt_service,
                oauth_config,
                logs_dir,
                websocket_service,
                github_app_configs,
                webhook_secret,
                github_client,
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
        // Determine the path to the frontend static files
        // When running from the project root, this should be "frontend/static"
        let static_dir = std::env::current_dir()
            .expect("Failed to get current directory")
            .join("frontend")
            .join("static");

        info!("Serving static files from: {:?}", static_dir);

        // Create static file service with fallback to index.html for SPA routing
        let serve_dir = ServeDir::new(&static_dir)
            .not_found_service(ServeFile::new(static_dir.join("index.html")));

        // Configure CORS to allow frontend requests
        let cors = CorsLayer::permissive();

        let app = Router::new()
            .nest("/v1", api_routes())
            .nest("/github", github_routes())
            .fallback_service(serve_dir)
            .layer(cors)
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
        // Pull Request routes
        .route("/prs", get(list_pull_requests_handler))
        .route("/prs/{owner}/{repo}/{pr_number}", get(get_pull_request_handler))
        .route("/prs/{owner}/{repo}/{pr_number}/github-metadata", get(get_pr_github_metadata_handler))
        // Merge Queue routes
        .route("/merge-queue/{owner}/{repo}", get(list_merge_queue_builds_handler))
        .route("/merge-queue/{owner}/{repo}/{sha}", get(get_merge_queue_build_handler))
        // Job and build status routes
        .route("/commits/{sha}/jobs", get(get_commit_jobs_handler))
        .route("/jobs/{jobset_id}", get(get_jobset_details_handler))
        .route("/jobs/{jobset_id}/drvs", get(get_jobset_drvs_handler))
        .route("/builds/active", get(get_active_builds_handler))
        // Derivation details routes
        .route("/drvs/{drv}", get(get_drv_details_handler))
        .route("/drvs/{drv}/dependencies", get(get_drv_dependencies_handler))
        .route("/drvs/{drv}/hooks", get(get_drv_hooks_handler))
        .route("/logs/{drv}/hooks/{hook_name}", get(get_hook_log))
        // User profile routes (authenticated)
        .route("/users/me/profile", get(user_profile_handler))
        .route("/users/me/profile", axum::routing::patch(update_user_profile_handler))
        .route("/users/me/maintained-paths", get(user_maintained_paths_handler))
        // Admin routes (protected)
        .route("/admin/approved-users", get(list_approved_users_handler))
        .route("/admin/approved-users", post(add_approved_user_handler))
        .route(
            "/admin/approved-users/{username}",
            axum::routing::delete(remove_approved_user_handler),
        )
        // Admin user management routes
        .route("/admin/users", get(admin_list_users_handler))
        .route("/admin/users/{github_id}/promote", post(admin_promote_user_handler))
        .route("/admin/users/{github_id}/demote", post(admin_demote_user_handler))
        .route("/admin/users/{github_id}", axum::routing::delete(admin_delete_user_handler))
        .route("/admin/users/{github_id}/maintained-paths", get(admin_user_maintained_paths_handler))
        // Admin attr path maintainer routes
        .route("/admin/attr-paths/{attr_path}/maintainers", post(admin_add_maintainer_handler))
        .route("/admin/attr-paths/{attr_path}/maintainers/{github_id}", axum::routing::delete(admin_remove_maintainer_handler))
        .route("/admin/attr-paths/{attr_path}/maintainers", get(admin_list_maintainers_handler))
        // Admin maintainer request routes
        .route("/admin/maintainer-requests", get(admin_list_pending_requests_handler))
        .route("/admin/maintainer-requests/{request_id}/approve", post(admin_approve_request_handler))
        .route("/admin/maintainer-requests/{request_id}/reject", post(admin_reject_request_handler))
        // Public maintainer routes (no auth required)
        .route("/attr-paths/{attr_path}/maintainers", get(get_attr_path_maintainers_handler))
        .route("/jobs/{job_id}/maintainers", get(get_job_maintainers_handler))
        // Authenticated maintainer request routes
        .route("/attr-paths/{attr_path}/request-maintainer", post(request_maintainer_handler))
        .route("/users/me/maintainer-requests", get(get_my_requests_handler))
}

async fn handle_github_webhook(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) {
    use octocrab::models::webhook_events::EventInstallation;
    use serde_json::Value;
    use tracing::warn;

    use crate::github::handle_webhook_payload;

    // Verify webhook signature if secret is configured
    if let Some(ref secret) = state.webhook_secret {
        // Extract signature header
        let signature = match headers.get("x-hub-signature-256") {
            Some(sig) => match sig.to_str() {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        event = "webhook_signature_invalid_header",
                        error = %e,
                        "Failed to parse X-Hub-Signature-256 header"
                    );
                    return;
                },
            },
            None => {
                warn!(
                    event = "webhook_signature_missing",
                    "Missing X-Hub-Signature-256 header on webhook request"
                );
                return;
            },
        };

        // Verify the signature
        if let Err(e) = verify_webhook_signature(secret, signature, &body) {
            warn!(
                event = "webhook_signature_verification_failed",
                error = %e,
                "Webhook signature verification failed - rejecting webhook"
            );
            return;
        }

        info!(
            event = "webhook_signature_verified",
            "Webhook signature verified successfully"
        );
    }

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
        state.merge_queue_require_approval,
        state.db_service,
        state.github_app_configs,
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

async fn get_hook_log(
    State(state): State<AppState>,
    axum::extract::Path((drv, hook_name)): axum::extract::Path<(String, String)>,
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

    // Construct the log file path: {logs_dir}/{drv_hash}/hook-{hook_name}.log
    let drv_hash = drv_id.drv_hash();
    let log_path = state
        .logs_dir
        .join(drv_hash)
        .join(format!("hook-{}.log", hook_name));

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

async fn get_drv_hooks_handler(
    State(state): State<AppState>,
    axum::extract::Path(drv): axum::extract::Path<String>,
) -> impl IntoResponse {
    use axum::http::StatusCode;
    use serde::Serialize;
    use tracing::warn;

    use crate::db::model::drv_id::DrvId;

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
    let drv_id = match DrvId::try_from(drv.as_str()) {
        Ok(id) => id,
        Err(e) => {
            warn!("Invalid drv format: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("Invalid derivation format: {}", e)
                })),
            )
                .into_response();
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
    match state
        .db_service
        .list_repository_commits(&owner, &repo, query.limit)
        .await
    {
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

    match state
        .db_service
        .get_jobset_drvs(jobset_id, state_filter, query.limit, query.offset)
        .await
    {
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

// Active builds handler
async fn get_active_builds_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    match state.db_service.get_active_jobs().await {
        Ok(jobs) => {
            // Also get all building drvs
            match state.db_service.get_all_building_drvs().await {
                Ok(building_drvs) => {
                    // Return jobs and all building drvs
                    Ok(Json(serde_json::json!({
                        "jobs": jobs,
                        "building_drvs": building_drvs,
                    })))
                },
                Err(e) => {
                    error!("Failed to get building drvs: {}", e);
                    // Return jobs even if we can't get building drvs
                    Ok(Json(serde_json::json!({
                        "jobs": jobs,
                        "building_drvs": [],
                    })))
                },
            }
        },
        Err(e) => {
            error!("Failed to get active jobs: {}", e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get active jobs: {}", e),
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
            ));
        },
    };

    // Use graph_handle for lockfree access to node data
    match state.graph_handle.get_node(&drv_id) {
        Some(node) => {
            // Get dependency count from the node
            let dep_count = node.dependencies.len() as i64;

            Ok(Json(serde_json::json!({
                "drv_path": node.drv_id,
                "system": node.system,
                "build_state": node.build_state,
                "is_fod": node.is_fod,
                "required_system_features": node.required_system_features,
                "dependency_count": dep_count,
            })))
        },
        None => Err((
            axum::http::StatusCode::NOT_FOUND,
            format!("Derivation not found: {}", drv),
        )),
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
            ));
        },
    };

    // Use graph_handle to get dependencies
    match state.graph_handle.get_dependencies(&drv_id).await {
        Ok(dep_ids) => {
            // For each dependency, get its details from the graph
            let mut dependencies = Vec::new();
            for dep_id in &dep_ids {
                if let Some(dep_node) = state.graph_handle.get_node(dep_id) {
                    dependencies.push(serde_json::json!({
                        "drv_path": dep_node.drv_id,
                        "system": dep_node.system,
                        "build_state": dep_node.build_state,
                    }));
                }
            }

            let count = dependencies.len() as i64;
            Ok(Json(serde_json::json!({
                "drv_path": drv,
                "dependencies": dependencies,
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
    ws.on_upgrade(
        move |socket| async move { state.websocket_service.handle_connection(socket).await },
    )
}

// ========== User Profile Handlers ==========

/// Get current user's profile
async fn user_profile_handler(auth: AuthUser, State(state): State<AppState>) -> impl IntoResponse {
    crate::auth::profile::get_profile(auth, State(state.db_service.pool.clone())).await
}

/// Update current user's profile
async fn update_user_profile_handler(
    auth: AuthUser,
    State(state): State<AppState>,
    body: Json<crate::auth::UpdateProfileRequest>,
) -> impl IntoResponse {
    crate::auth::profile::update_profile(auth, State(state.db_service.pool.clone()), body).await
}

/// Get attr paths maintained by current user
async fn user_maintained_paths_handler(
    auth: AuthUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::profile::get_maintained_paths(auth, State(state.db_service.pool.clone())).await
}

// ========== Admin User Management Handlers ==========

/// List all users (admin only)
async fn admin_list_users_handler(
    admin: AdminUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::admin::list_users(admin, State(state.db_service.pool.clone())).await
}

/// Promote a user to admin
async fn admin_promote_user_handler(
    admin: AdminUser,
    Path(github_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::admin::promote_user(admin, Path(github_id), State(state.db_service.pool.clone()))
        .await
}

/// Demote a user from admin
async fn admin_demote_user_handler(
    admin: AdminUser,
    Path(github_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::admin::demote_user(admin, Path(github_id), State(state.db_service.pool.clone()))
        .await
}

/// Delete a user
async fn admin_delete_user_handler(
    admin: AdminUser,
    Path(github_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::admin::delete_user(admin, Path(github_id), State(state.db_service.pool.clone()))
        .await
}

/// Get attr paths maintained by a specific user (admin only)
async fn admin_user_maintained_paths_handler(
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

// ========== Admin Attr Path Maintainer Handlers ==========

/// Add a maintainer to an attr path
async fn admin_add_maintainer_handler(
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

/// Remove a maintainer from an attr path
async fn admin_remove_maintainer_handler(
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

/// List maintainers for an attr path
async fn admin_list_maintainers_handler(
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

// ============================================================================
// Pull Request Handlers
// ============================================================================

/// List all open pull requests
async fn list_pull_requests_handler(State(state): State<AppState>) -> impl IntoResponse {
    match state.db_service.list_open_pull_requests().await {
        Ok(prs) => Json(prs).into_response(),
        Err(e) => {
            error!("Failed to list pull requests: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to list pull requests: {}", e),
            )
                .into_response()
        },
    }
}

/// Get a specific pull request with build statistics
async fn get_pull_request_handler(
    State(state): State<AppState>,
    Path((owner, repo, pr_number)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    match state
        .db_service
        .get_pull_request(&owner, &repo, pr_number)
        .await
    {
        Ok(Some(pr)) => Json(pr).into_response(),
        Ok(None) => (
            axum::http::StatusCode::NOT_FOUND,
            format!(
                "Pull request #{} not found in {}/{}",
                pr_number, owner, repo
            ),
        )
            .into_response(),
        Err(e) => {
            error!("Failed to get pull request: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get pull request: {}", e),
            )
                .into_response()
        },
    }
}

#[derive(Serialize, Deserialize)]
struct GitHubPRMetadata {
    additions: i64,
    deletions: i64,
    changed_files: i64,
}

/// Get GitHub API metadata for a pull request (lines changed, etc.)
async fn get_pr_github_metadata_handler(
    State(state): State<AppState>,
    Path((owner, repo, pr_number)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    let octocrab = match &state.octocrab {
        Some(o) => o,
        None => {
            return (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "GitHub API not available",
            )
                .into_response();
        },
    };

    match octocrab.pulls(&owner, &repo).get(pr_number as u64).await {
        Ok(pr) => {
            let metadata = GitHubPRMetadata {
                additions: pr.additions.unwrap_or(0) as i64,
                deletions: pr.deletions.unwrap_or(0) as i64,
                changed_files: pr.changed_files.unwrap_or(0) as i64,
            };
            Json(metadata).into_response()
        },
        Err(e) => {
            error!("Failed to fetch PR metadata from GitHub: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to fetch PR metadata: {}", e),
            )
                .into_response()
        },
    }
}

// ============================================================================
// Merge Queue Handlers
// ============================================================================

/// List all merge queue builds for a repository
async fn list_merge_queue_builds_handler(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
) -> impl IntoResponse {
    match state
        .db_service
        .list_merge_queue_builds(&owner, &repo)
        .await
    {
        Ok(builds) => Json(builds).into_response(),
        Err(e) => {
            error!("Failed to list merge queue builds: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to list merge queue builds: {}", e),
            )
                .into_response()
        },
    }
}

/// Get a specific merge queue build by commit SHA
async fn get_merge_queue_build_handler(
    State(state): State<AppState>,
    Path((owner, repo, sha)): Path<(String, String, String)>,
) -> impl IntoResponse {
    match state
        .db_service
        .get_merge_queue_build_by_sha(&owner, &repo, &sha)
        .await
    {
        Ok(Some(build)) => Json(build).into_response(),
        Ok(None) => (
            axum::http::StatusCode::NOT_FOUND,
            format!(
                "Merge queue build not found for commit {} in {}/{}",
                sha, owner, repo
            ),
        )
            .into_response(),
        Err(e) => {
            error!("Failed to get merge queue build: {}", e);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get merge queue build: {}", e),
            )
                .into_response()
        },
    }
}

// ============================================================================
// Maintainer Request Handlers
// ============================================================================

/// Get all maintainers for an attr path (public endpoint)
async fn get_attr_path_maintainers_handler(
    Path(attr_path): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::get_attr_path_maintainers(
        Path(attr_path),
        State(state.db_service.pool.clone()),
    )
    .await
}

/// Get all maintainers for a job (public endpoint)
async fn get_job_maintainers_handler(
    Path(job_id): Path<i64>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::get_job_maintainers(Path(job_id), State(state.db_service.pool.clone()))
        .await
}

/// Request to become a maintainer (authenticated endpoint)
async fn request_maintainer_handler(
    user: AuthUser,
    Path(attr_path): Path<String>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let request_state = crate::auth::RequestHandlerState {
        pool: state.db_service.pool.clone(),
        github_client: state.github_client.clone(),
    };

    crate::auth::requests::request_maintainer(user, Path(attr_path), State(request_state)).await
}

/// Get current user's maintainer requests (authenticated endpoint)
async fn get_my_requests_handler(
    user: AuthUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::get_my_requests(user, State(state.db_service.pool.clone())).await
}

/// List all pending maintainer requests (admin only)
async fn admin_list_pending_requests_handler(
    admin: AdminUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::list_pending_requests(admin, State(state.db_service.pool.clone())).await
}

/// Approve a maintainer request (admin only)
async fn admin_approve_request_handler(
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

/// Reject a maintainer request (admin only)
async fn admin_reject_request_handler(
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
