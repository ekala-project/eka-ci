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
use tracing::{error, info, warn};

use crate::auth::{AdminUser, AuthUser, JwtService, OAuthConfig};
use crate::db::github::CheckRun;
use crate::git::GitTask;
use crate::github::GitHubTask;
use crate::scheduler::IngressTask;
use crate::webhook_security::{check_webhook_secret_configured, verify_webhook_signature};

#[derive(Clone)]
struct AppState {
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    ingress_sender: Option<mpsc::Sender<IngressTask>>,
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
    default_merge_method: String,
}

// Implement FromRef so extractors can access JwtService from AppState
impl axum::extract::FromRef<AppState> for JwtService {
    fn from_ref(state: &AppState) -> Self {
        state.jwt_service.clone()
    }
}

impl AppState {
    /// Build the narrower `crate::auth::oauth::AppState` used by OAuth handlers.
    fn oauth_state(&self) -> crate::auth::oauth::AppState {
        crate::auth::oauth::AppState {
            db: self.db_service.pool.clone(),
            jwt_service: self.jwt_service.clone(),
            oauth_config: self.oauth_config.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Small response helpers
//
// These collapse the verbose `(StatusCode::X, "msg").into_response()` pattern
// used throughout this file into self-documenting calls, keeping handler
// happy-paths visually prominent.
// ---------------------------------------------------------------------------

fn bad_request(msg: impl Into<String>) -> axum::response::Response {
    (axum::http::StatusCode::BAD_REQUEST, msg.into()).into_response()
}

fn not_found(msg: impl Into<String>) -> axum::response::Response {
    (axum::http::StatusCode::NOT_FOUND, msg.into()).into_response()
}

fn internal_error(msg: impl Into<String>) -> axum::response::Response {
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg.into()).into_response()
}

fn forbidden(msg: impl Into<String>) -> axum::response::Response {
    (axum::http::StatusCode::FORBIDDEN, msg.into()).into_response()
}

fn service_unavailable(msg: impl Into<String>) -> axum::response::Response {
    (axum::http::StatusCode::SERVICE_UNAVAILABLE, msg.into()).into_response()
}

/// Parse a `DrvId` from a string, returning a `(status, message)` pair on
/// failure that is suitable for `?`-propagation in handlers returning
/// `Result<_, (StatusCode, String)>`.
fn parse_drv_id(
    drv: &str,
) -> Result<crate::db::model::drv_id::DrvId, (axum::http::StatusCode, String)> {
    crate::db::model::drv_id::DrvId::try_from(drv).map_err(|e| {
        warn!("Invalid drv format: {}", e);
        (
            axum::http::StatusCode::BAD_REQUEST,
            format!("Invalid derivation format: {}", e),
        )
    })
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
        ingress_sender: Option<mpsc::Sender<IngressTask>>,
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
        default_merge_method: String,
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
                ingress_sender,
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
                default_merge_method,
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
        // Auto-merge routes (authenticated)
        .route("/prs/{owner}/{repo}/{pr_number}/enable-auto-merge", post(enable_auto_merge_handler))
        .route("/prs/{owner}/{repo}/{pr_number}/disable-auto-merge", post(disable_auto_merge_handler))
        .route("/prs/{owner}/{repo}/{pr_number}/merge", post(manual_merge_pr_handler))
        .route("/prs/{owner}/{repo}/{pr_number}/merge-eligibility", get(check_merge_eligibility_handler))
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
        .route("/admin/attr-paths/{attr_path}/maintainers/by-username", post(admin_add_maintainer_by_username_handler))
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
        .route("/jobs/{job_id}/request-maintainer", post(request_maintainer_for_job_handler))
        .route("/users/me/maintainer-requests", get(get_my_requests_handler))
        .route("/maintainer-requests/{request_id}", get(get_maintainer_request_handler))
        // Build-control routes (gated on maintainer status of all attr paths the
        // drv belongs to, or admin)
        .route("/drvs/{drv}/rebuild", post(rebuild_drv_handler))
        .route("/admin/rebuild-all-failed", post(admin_rebuild_all_failed_handler))
        // Admin GitHub API permission-cache maintenance
        .route(
            "/admin/github-api-cache/clear",
            post(admin_github_api_cache_clear_handler),
        )
        .route(
            "/admin/github-api-cache/invalidate",
            post(admin_github_api_cache_invalidate_handler),
        )
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

    // Parse the drv parameter into a DrvId
    let drv_id = match parse_drv_id(&drv) {
        Ok(id) => id,
        Err((status, msg)) => return (status, msg).into_response(),
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

// Auth handlers
async fn auth_login_handler(State(state): State<AppState>) -> impl IntoResponse {
    crate::auth::handle_login(State(state.oauth_state())).await
}

async fn auth_callback_handler(
    query: axum::extract::Query<crate::auth::oauth::CallbackParams>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::handle_callback(query, State(state.oauth_state())).await
}

async fn auth_me_handler(user: AuthUser, State(state): State<AppState>) -> impl IntoResponse {
    crate::auth::handle_me(user, State(state.oauth_state())).await
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
    let drv_id = parse_drv_id(&drv)?;

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
    let drv_id = parse_drv_id(&drv)?;

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
            internal_error(format!("Failed to list pull requests: {}", e))
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
        Ok(None) => not_found(format!(
            "Pull request #{} not found in {}/{}",
            pr_number, owner, repo
        )),
        Err(e) => {
            error!("Failed to get pull request: {}", e);
            internal_error(format!("Failed to get pull request: {}", e))
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
        None => return service_unavailable("GitHub API not available"),
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
            internal_error(format!("Failed to fetch PR metadata: {}", e))
        },
    }
}

// ============================================================================
// Auto-Merge Handlers
// ============================================================================

/// Ensure the authenticated user is a maintainer of every package changed by
/// the referenced PR. Returns the changed package list on success, or a ready
/// `Response` describing the failure that the caller should return directly.
///
/// `action_verb_phrase` is a short phrase (e.g. "enable auto-merge") that is
/// embedded into the `403 Forbidden` body when the user is not a maintainer.
async fn ensure_pr_maintainer_access(
    auth_user: &AuthUser,
    owner: &str,
    repo: &str,
    pr_number: i64,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    action_verb_phrase: &str,
) -> Result<Vec<String>, axum::response::Response> {
    // Get changed packages for this PR
    let changed_packages = crate::db::github::get_pr_changed_packages(pr_number, owner, repo, pool)
        .await
        .map_err(|e| {
            error!("Failed to get changed packages for PR: {}", e);
            internal_error(format!("Failed to get changed packages: {}", e))
        })?;

    if changed_packages.is_empty() {
        return Err(bad_request("No changed packages found for this PR"));
    }

    // Check if user is maintainer of ALL changed packages
    let is_maintainer = crate::db::maintainers::is_maintainer_of_all_packages(
        auth_user.github_id,
        &changed_packages,
        pool,
    )
    .await
    .map_err(|e| {
        error!("Failed to check maintainer status: {}", e);
        internal_error(format!("Failed to check maintainer status: {}", e))
    })?;

    if !is_maintainer {
        return Err(forbidden(format!(
            "You must be a maintainer of all changed packages to {}",
            action_verb_phrase
        )));
    }

    Ok(changed_packages)
}

#[derive(Debug, serde::Deserialize)]
struct EnableAutoMergeRequest {
    merge_method: Option<String>,
}

/// Enable auto-merge for a pull request (maintainers only)
async fn enable_auto_merge_handler(
    State(state): State<AppState>,
    auth_user: crate::auth::AuthUser,
    Path((owner, repo, pr_number)): Path<(String, String, i64)>,
    Json(payload): Json<EnableAutoMergeRequest>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_pr_maintainer_access(
        &auth_user,
        &owner,
        &repo,
        pr_number,
        &state.db_service.pool,
        "enable auto-merge",
    )
    .await
    {
        return resp;
    }

    // Validate merge method if provided
    if let Some(ref method) = payload.merge_method {
        if method != "merge" && method != "squash" && method != "rebase" {
            return bad_request("Invalid merge method. Must be 'merge', 'squash', or 'rebase'");
        }
    }

    // Enable auto-merge
    match crate::db::github::enable_auto_merge(
        &owner,
        &repo,
        pr_number,
        payload.merge_method.as_deref(),
        &state.db_service.pool,
    )
    .await
    {
        Ok(_) => {
            info!(
                "Auto-merge enabled for PR #{} in {}/{} by user {}",
                pr_number, owner, repo, auth_user.claims.username
            );
            (axum::http::StatusCode::OK, "Auto-merge enabled").into_response()
        },
        Err(e) => {
            error!("Failed to enable auto-merge: {}", e);
            internal_error(format!("Failed to enable auto-merge: {}", e))
        },
    }
}

/// Disable auto-merge for a pull request (maintainers only)
async fn disable_auto_merge_handler(
    State(state): State<AppState>,
    auth_user: crate::auth::AuthUser,
    Path((owner, repo, pr_number)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    if let Err(resp) = ensure_pr_maintainer_access(
        &auth_user,
        &owner,
        &repo,
        pr_number,
        &state.db_service.pool,
        "disable auto-merge",
    )
    .await
    {
        return resp;
    }

    // Disable auto-merge
    match crate::db::github::disable_auto_merge(&owner, &repo, pr_number, &state.db_service.pool)
        .await
    {
        Ok(_) => {
            info!(
                "Auto-merge disabled for PR #{} in {}/{} by user {}",
                pr_number, owner, repo, auth_user.claims.username
            );
            (axum::http::StatusCode::OK, "Auto-merge disabled").into_response()
        },
        Err(e) => {
            error!("Failed to disable auto-merge: {}", e);
            internal_error(format!("Failed to disable auto-merge: {}", e))
        },
    }
}

#[derive(Debug, serde::Deserialize)]
struct ManualMergeRequest {
    merge_method: Option<String>,
}

/// Manually merge a pull request (maintainers only)
async fn manual_merge_pr_handler(
    State(state): State<AppState>,
    auth_user: crate::auth::AuthUser,
    Path((owner, repo, pr_number)): Path<(String, String, i64)>,
    Json(payload): Json<ManualMergeRequest>,
) -> impl IntoResponse {
    let octocrab = match &state.octocrab {
        Some(o) => o,
        None => return service_unavailable("GitHub API not available"),
    };

    if let Err(resp) = ensure_pr_maintainer_access(
        &auth_user,
        &owner,
        &repo,
        pr_number,
        &state.db_service.pool,
        "merge",
    )
    .await
    {
        return resp;
    }

    // Validate merge method
    let merge_method = payload
        .merge_method
        .as_deref()
        .or(Some(state.default_merge_method.as_str()))
        .unwrap_or("squash");

    if merge_method != "merge" && merge_method != "squash" && merge_method != "rebase" {
        return bad_request("Invalid merge method. Must be 'merge', 'squash', or 'rebase'");
    }

    // Validate that the chosen method is actually allowed by the repository
    // settings before calling the merge API. Surface a 409 so the client
    // can see both what they asked for and what the repo permits.
    match crate::github::service::actions::validate_merge_method(
        octocrab,
        &owner,
        &repo,
        merge_method,
    )
    .await
    {
        Ok(crate::github::service::actions::MergeMethodCheck::Ok) => {},
        Ok(crate::github::service::actions::MergeMethodCheck::NotAllowed { allowed }) => {
            return (
                axum::http::StatusCode::CONFLICT,
                format!(
                    "Merge method '{}' is not allowed by repository settings. Allowed methods: {}",
                    merge_method,
                    if allowed.is_empty() {
                        "<none>".to_string()
                    } else {
                        allowed.join(", ")
                    }
                ),
            )
                .into_response();
        },
        Err(e) => {
            error!("Failed to fetch repository merge settings: {}", e);
            return internal_error(format!(
                "Failed to validate merge method against repository settings: {}",
                e
            ));
        },
    }

    // Attempt to merge
    match crate::github::service::actions::merge_pull_request(
        octocrab,
        &owner,
        &repo,
        pr_number as u64,
        merge_method,
        None,
        None,
    )
    .await
    {
        Ok(_) => {
            info!(
                "PR #{} in {}/{} manually merged by user {}",
                pr_number, owner, repo, auth_user.claims.username
            );

            // Mark PR as merged in database
            // TODO: Look up user's database ID from github_id
            if let Err(e) = crate::db::github::mark_pr_merged(
                &owner,
                &repo,
                pr_number,
                None, // TODO: Pass actual user ID
                &state.db_service.pool,
            )
            .await
            {
                warn!("Failed to mark PR as merged in database: {}", e);
            }

            (axum::http::StatusCode::OK, "PR merged successfully").into_response()
        },
        Err(e) => {
            error!("Failed to merge PR: {}", e);
            internal_error(format!("Failed to merge PR: {}", e))
        },
    }
}

#[derive(Debug, serde::Serialize)]
struct MergeEligibility {
    eligible: bool,
    auto_merge_enabled: bool,
    gates_passed: bool,
    has_maintainer_approvals: bool,
    changed_packages: Vec<String>,
    missing_approvals: std::collections::HashMap<String, Vec<String>>,
}

/// Check if a PR is eligible for auto-merge
async fn check_merge_eligibility_handler(
    State(state): State<AppState>,
    Path((owner, repo, pr_number)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    let octocrab = match &state.octocrab {
        Some(o) => o,
        None => return service_unavailable("GitHub API not available"),
    };

    // Get PR from database
    let pr = match sqlx::query_as::<_, crate::db::github::PullRequest>(
        "SELECT * FROM PullRequests WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(&owner)
    .bind(&repo)
    .bind(pr_number)
    .fetch_optional(&state.db_service.pool)
    .await
    {
        Ok(Some(pr)) => pr,
        Ok(None) => return not_found("Pull request not found"),
        Err(e) => {
            error!("Failed to fetch PR: {}", e);
            return internal_error(format!("Failed to fetch PR: {}", e));
        },
    };

    // Get changed packages
    let changed_packages = match crate::db::github::get_pr_changed_packages(
        pr_number,
        &owner,
        &repo,
        &state.db_service.pool,
    )
    .await
    {
        Ok(packages) => packages,
        Err(e) => {
            error!("Failed to get changed packages: {}", e);
            return internal_error(format!("Failed to get changed packages: {}", e));
        },
    };

    // Check if gates passed (all jobs concluded successfully)
    let gates_passed = if let Some(jobset_id) = pr.jobset_id {
        match state.db_service.all_jobs_concluded(jobset_id).await {
            Ok(true) => {
                match state
                    .db_service
                    .jobset_has_new_or_changed_failures(jobset_id)
                    .await
                {
                    Ok(has_failures) => !has_failures,
                    Err(e) => {
                        error!("Failed to check for failures: {}", e);
                        false
                    },
                }
            },
            Ok(false) => false,
            Err(e) => {
                error!("Failed to check if jobs concluded: {}", e);
                false
            },
        }
    } else {
        false
    };

    // Check maintainer approvals
    let (has_maintainer_approvals, missing_approvals) =
        match crate::github::service::actions::check_pr_maintainer_approvals(
            octocrab,
            &owner,
            &repo,
            pr_number as u64,
            &changed_packages,
            &state.db_service.pool,
        )
        .await
        {
            Ok(result) => result,
            Err(e) => {
                error!("Failed to check maintainer approvals: {}", e);
                (false, std::collections::HashMap::new())
            },
        };

    let eligible = pr.auto_merge_enabled && gates_passed && has_maintainer_approvals;

    Json(MergeEligibility {
        eligible,
        auto_merge_enabled: pr.auto_merge_enabled,
        gates_passed,
        has_maintainer_approvals,
        changed_packages,
        missing_approvals,
    })
    .into_response()
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
            internal_error(format!("Failed to list merge queue builds: {}", e))
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
        Ok(None) => not_found(format!(
            "Merge queue build not found for commit {} in {}/{}",
            sha, owner, repo
        )),
        Err(e) => {
            error!("Failed to get merge queue build: {}", e);
            internal_error(format!("Failed to get merge queue build: {}", e))
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
    body: Option<Json<crate::auth::RequestMaintainerRequest>>,
) -> impl IntoResponse {
    let request_state = crate::auth::RequestHandlerState {
        pool: state.db_service.pool.clone(),
        github_client: state.github_client.clone(),
    };

    crate::auth::requests::request_maintainer(user, Path(attr_path), State(request_state), body)
        .await
}

/// Request to become a maintainer of the attr path of a given job
/// (authenticated endpoint). Resolves job → attr path before delegating.
async fn request_maintainer_for_job_handler(
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

/// Get current user's maintainer requests (authenticated endpoint)
async fn get_my_requests_handler(
    user: AuthUser,
    State(state): State<AppState>,
) -> impl IntoResponse {
    crate::auth::requests::get_my_requests(user, State(state.db_service.pool.clone())).await
}

/// Get a specific maintainer request by id (authenticated endpoint; owner
/// or admin only).
async fn get_maintainer_request_handler(
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

/// Admin: add a maintainer to an attr path by GitHub username.
async fn admin_add_maintainer_by_username_handler(
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

/// Ensure the authenticated user can modify builds for the given derivation:
/// either an admin, or a maintainer of every attr path the derivation is
/// associated with via `Job` rows. Returns the list of attr paths on success
/// or a ready `Response` describing the failure.
async fn ensure_drv_maintainer_access(
    auth_user: &AuthUser,
    drv_path: &str,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    action_verb_phrase: &str,
) -> Result<Vec<String>, axum::response::Response> {
    // Admins can modify any build regardless of attr-path association.
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

    // The user must be able to modify every attr path this drv participates in.
    // `can_modify_build` short-circuits for admins and otherwise delegates to
    // `is_maintainer_of`, so this loop exercises both helpers.
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

/// Rebuild a failed derivation (admin or maintainer of all associated attr
/// paths). The derivation must currently be in a failed state; otherwise the
/// scheduler will log a warning and take no action.
async fn rebuild_drv_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
    Path(drv): Path<String>,
) -> impl IntoResponse {
    let drv_id = match parse_drv_id(&drv) {
        Ok(id) => id,
        Err((status, msg)) => return (status, msg).into_response(),
    };

    // Confirm the derivation exists before doing any authorization work so we
    // can return a clean 404 rather than a 403 for unknown drvs.
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
        .send(IngressTask::RebuildFailed(drv_id.clone()))
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

/// Admin-only: rebuild every failed derivation in the system.
async fn admin_rebuild_all_failed_handler(
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

#[derive(Deserialize)]
struct InvalidateGitHubApiCacheRequest {
    /// GitHub user id whose cached permission entry should be invalidated.
    github_id: i64,
    owner: String,
    repo: String,
}

/// Admin-only: drop every entry in the in-memory GitHub permission cache.
///
/// The cache normally expires entries after 10 minutes; this endpoint is for
/// operators who need an immediate refresh (e.g. after updating collaborator
/// access on GitHub and wanting the change reflected before the TTL elapses).
async fn admin_github_api_cache_clear_handler(
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

/// Admin-only: invalidate a single `(user, owner, repo)` entry in the GitHub
/// permission cache. The target user is identified by `github_id`; the handler
/// looks up their stored access token so admins don't need to handle raw
/// tokens.
async fn admin_github_api_cache_invalidate_handler(
    State(state): State<AppState>,
    admin: AdminUser,
    Json(body): Json<InvalidateGitHubApiCacheRequest>,
) -> impl IntoResponse {
    // Look up the target user's access token. The cache is keyed on
    // `access_token:owner:repo`, so invalidation requires the actual token the
    // entry was cached under.
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
