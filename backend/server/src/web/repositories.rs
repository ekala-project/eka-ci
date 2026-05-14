// Repository, job, and derivation handlers

use axum::extract::{Json, Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use tracing::{debug, error};

use super::security::parse_drv_id;
use super::state::AppState;
use crate::auth::AuthUser;
use crate::db::github::CheckRun;

pub(super) async fn get_check_runs_for_commit(
    _auth: AuthUser,
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

pub(super) async fn list_repositories_handler(
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

pub(super) async fn get_repository_handler(
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
pub(super) struct CommitsQuery {
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    20
}

pub(super) async fn list_repository_commits_handler(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    Query(query): Query<CommitsQuery>,
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

#[derive(Deserialize)]
pub(super) struct JobsetsQuery {
    #[serde(default = "default_jobsets_limit")]
    limit: i64,
    #[serde(default = "default_sort_desc")]
    sort_desc: bool,
}

fn default_jobsets_limit() -> i64 {
    50
}

fn default_sort_desc() -> bool {
    true
}

pub(super) async fn get_repository_jobsets_handler(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    Query(query): Query<JobsetsQuery>,
) -> Result<Json<Vec<crate::db::github::RepositoryJobSetSummary>>, (axum::http::StatusCode, String)>
{
    use crate::db::github::get_repository_jobsets;

    match get_repository_jobsets(
        &owner,
        &repo,
        query.limit,
        query.sort_desc,
        &state.db_service.pool,
    )
    .await
    {
        Ok(jobsets) => Ok(Json(jobsets)),
        Err(e) => {
            error!("Failed to get jobsets for {}/{}: {}", owner, repo, e);
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to get jobsets: {}", e),
            ))
        },
    }
}

pub(super) async fn get_commit_jobs_handler(
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

pub(super) async fn get_jobset_details_handler(
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
pub(super) struct DrvsQuery {
    #[serde(default = "default_drv_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    state: Option<String>,
}

fn default_drv_limit() -> i64 {
    100
}

pub(super) async fn get_jobset_drvs_handler(
    State(state): State<AppState>,
    Path(jobset_id): Path<i64>,
    Query(query): Query<DrvsQuery>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    use std::str::FromStr;

    use crate::db::model::build_event::DrvBuildState;

    let state_filter = match query.state {
        Some(state_str) => match DrvBuildState::from_str(&state_str) {
            Ok(s) => Some(s),
            Err(e) => {
                return Err((
                    axum::http::StatusCode::BAD_REQUEST,
                    format!("Invalid state parameter: {}", e),
                ));
            },
        },
        None => None,
    };

    match state
        .db_service
        .get_jobset_drvs(jobset_id, state_filter, query.limit, query.offset)
        .await
    {
        Ok(drvs) => match state.db_service.count_jobset_drvs(jobset_id).await {
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

#[derive(Deserialize)]
pub(super) struct PackageChangesQuery {
    base_sha: String,
    job: String,
    #[serde(default)]
    max_packages_listed: Option<usize>,
}

pub(super) async fn get_package_changes_handler(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(sha): Path<String>,
    Query(query): Query<PackageChangesQuery>,
) -> Result<Json<crate::change_summary_compat::PackageChangesResponse>, (axum::http::StatusCode, String)> {
    use crate::change_summary_compat::{DEFAULT_MAX_PACKAGES_LISTED, build_package_changes_response};

    let max_listed = query
        .max_packages_listed
        .unwrap_or(DEFAULT_MAX_PACKAGES_LISTED)
        .min(DEFAULT_MAX_PACKAGES_LISTED * 10)
        .max(1);

    match build_package_changes_response(
        &state.db_service.pool,
        &sha,
        &query.base_sha,
        &query.job,
        max_listed,
    )
    .await
    {
        Ok(Some(resp)) => Ok(Json(resp)),
        Ok(None) => Err((
            axum::http::StatusCode::NOT_FOUND,
            format!("No jobset found for sha={sha} job={}", query.job),
        )),
        Err(e) => {
            error!(
                "Failed to compute package changes for sha={} job={}: {}",
                sha, query.job, e
            );
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to compute package changes: {e}"),
            ))
        },
    }
}

#[derive(Deserialize)]
pub(super) struct RebuildImpactQuery {
    base_sha: String,
    job: String,
    #[serde(default)]
    max_top_blast_radius: Option<usize>,
}

pub(super) async fn get_rebuild_impact_handler(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(sha): Path<String>,
    Query(query): Query<RebuildImpactQuery>,
) -> Result<Json<crate::change_summary_compat::RebuildImpactResponse>, (axum::http::StatusCode, String)> {
    use crate::change_summary_compat::impact::{
        DEFAULT_MAX_TOP_BLAST_RADIUS, build_rebuild_impact_response_cached,
    };

    let top_k = query
        .max_top_blast_radius
        .unwrap_or(DEFAULT_MAX_TOP_BLAST_RADIUS)
        .min(DEFAULT_MAX_TOP_BLAST_RADIUS * 10)
        .max(1);

    // TODO: Pass actual metrics when change_summary crate supports server's metrics type
    match build_rebuild_impact_response_cached(
        &state.db_service.pool,
        &state.graph_handle,
        &sha,
        &query.base_sha,
        &query.job,
        top_k,
        false,
        None, // metrics not supported yet
    )
    .await
    {
        Ok(Some(resp)) => Ok(Json(resp)),
        Ok(None) => Err((
            axum::http::StatusCode::NOT_FOUND,
            format!("No jobset found for sha={sha} job={}", query.job),
        )),
        Err(e) => {
            error!(
                "Failed to compute rebuild impact for sha={} job={}: {}",
                sha, query.job, e
            );
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to compute rebuild impact: {e}"),
            ))
        },
    }
}

#[derive(Deserialize)]
pub(super) struct ChangeSummaryQuery {
    base_sha: String,
    job: String,
    #[serde(default)]
    max_packages_listed: Option<usize>,
    #[serde(default)]
    max_top_blast_radius: Option<usize>,
}

pub(super) async fn get_change_summary_handler(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(sha): Path<String>,
    Query(query): Query<ChangeSummaryQuery>,
) -> Result<Json<crate::change_summary_compat::ChangeSummary>, (axum::http::StatusCode, String)> {
    use crate::change_summary_compat::{
        DEFAULT_MAX_PACKAGES_LISTED, DEFAULT_MAX_TOP_BLAST_RADIUS,
        build_change_summary, resolve_options_for_jobset,
    };

    let (base_opts, status) = resolve_options_for_jobset(
        &state.db_service.pool,
        &sha,
        &query.job,
        state.change_summary_metrics.as_deref(),
    )
    .await;

    let opts = crate::change_summary_compat::ChangeSummaryOptions {
        max_packages_listed: query
            .max_packages_listed
            .unwrap_or(DEFAULT_MAX_PACKAGES_LISTED)
            .min(DEFAULT_MAX_PACKAGES_LISTED * 10)
            .max(1),
        max_top_blast_radius: query
            .max_top_blast_radius
            .unwrap_or(DEFAULT_MAX_TOP_BLAST_RADIUS)
            .min(DEFAULT_MAX_TOP_BLAST_RADIUS * 10)
            .max(1),
        ..base_opts
    };

    match build_change_summary(
        &state.db_service.pool,
        &state.graph_handle,
        &sha,
        &query.base_sha,
        &query.job,
        &opts,
        &status,
        state.change_summary_metrics.as_deref(),
    )
    .await
    {
        Ok(Some(resp)) => Ok(Json(resp)),
        Ok(None) => Err((
            axum::http::StatusCode::NOT_FOUND,
            format!("No jobset found for sha={sha} job={}", query.job),
        )),
        Err(e) => {
            error!(
                "Failed to build change summary for sha={} job={}: {}",
                sha, query.job, e
            );
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to build change summary: {e}"),
            ))
        },
    }
}

pub(super) async fn get_change_summary_markdown_handler(
    State(state): State<AppState>,
    Path(sha): Path<String>,
    Query(query): Query<ChangeSummaryQuery>,
) -> Result<Response, (axum::http::StatusCode, String)> {
    use crate::change_summary_compat::{
        DEFAULT_MAX_PACKAGES_LISTED, DEFAULT_MAX_TOP_BLAST_RADIUS,
        build_change_summary, resolve_options_for_jobset,
    };

    let (base_opts, status) = resolve_options_for_jobset(
        &state.db_service.pool,
        &sha,
        &query.job,
        state.change_summary_metrics.as_deref(),
    )
    .await;

    let opts = crate::change_summary_compat::ChangeSummaryOptions {
        max_packages_listed: query
            .max_packages_listed
            .unwrap_or(DEFAULT_MAX_PACKAGES_LISTED)
            .min(DEFAULT_MAX_PACKAGES_LISTED * 10)
            .max(1),
        max_top_blast_radius: query
            .max_top_blast_radius
            .unwrap_or(DEFAULT_MAX_TOP_BLAST_RADIUS)
            .min(DEFAULT_MAX_TOP_BLAST_RADIUS * 10)
            .max(1),
        ..base_opts
    };

    match build_change_summary(
        &state.db_service.pool,
        &state.graph_handle,
        &sha,
        &query.base_sha,
        &query.job,
        &opts,
        &status,
        state.change_summary_metrics.as_deref(),
    )
    .await
    {
        Ok(Some(resp)) => Ok((
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            resp.markdown,
        )
            .into_response()),
        Ok(None) => Err((
            axum::http::StatusCode::NOT_FOUND,
            format!("No jobset found for sha={sha} job={}", query.job),
        )),
        Err(e) => {
            error!(
                "Failed to render change summary for sha={} job={}: {}",
                sha, query.job, e
            );
            Err((
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to render change summary: {e}"),
            ))
        },
    }
}

pub(super) async fn get_active_builds_handler(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    match state.db_service.get_active_jobs().await {
        Ok(jobs) => match state.db_service.get_all_building_drvs().await {
            Ok(building_drvs) => Ok(Json(serde_json::json!({
                "jobs": jobs,
                "building_drvs": building_drvs,
            }))),
            Err(e) => {
                error!("Failed to get building drvs: {}", e);
                Ok(Json(serde_json::json!({
                    "jobs": jobs,
                    "building_drvs": [],
                })))
            },
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

pub(super) async fn get_drv_details_handler(
    State(state): State<AppState>,
    Path(drv): Path<String>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let drv_id = parse_drv_id(&drv)?;
    let shared_drv_id = crate::graph_compat::to_shared_drv_id(&drv_id)
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match state.graph_handle.get_node(&shared_drv_id) {
        Some(node) => {
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

pub(super) async fn get_drv_dependencies_handler(
    State(state): State<AppState>,
    Path(drv): Path<String>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, String)> {
    let drv_id = parse_drv_id(&drv)?;
    let shared_drv_id = crate::graph_compat::to_shared_drv_id(&drv_id)
        .map_err(|e| (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    match state.graph_handle.get_dependencies(&shared_drv_id).await {
        Ok(dep_ids) => {
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

#[derive(Deserialize, Default)]
pub(super) struct WsAuthQuery {
    token: Option<String>,
}

pub(super) async fn websocket_handler(
    ws: axum::extract::ws::WebSocketUpgrade,
    Query(query): Query<WsAuthQuery>,
    headers: axum::http::HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let is_authenticated = match crate::auth::authenticate_request(
        &state.jwt_service,
        &headers,
        query.token.as_deref(),
    ) {
        Ok(_claims) => {
            debug!(event = "ws_upgrade_authenticated");
            true
        },
        Err(_) => {
            debug!(event = "ws_upgrade_unauthenticated");
            false
        },
    };

    ws.on_upgrade(move |socket| async move {
        if is_authenticated {
            debug!("WebSocket connection established (authenticated)");
        } else {
            debug!("WebSocket connection established (unauthenticated)");
        }
        state.websocket_service.handle_connection(socket).await
    })
    .into_response()
}
