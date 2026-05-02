// Router configuration for all API endpoints

use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;

use super::handlers::*;
use super::logs::*;
use super::maintainers::*;
use super::pull_requests::*;
use super::repositories::*;
use super::state::AppState;
use super::webhooks::*;

const WEBHOOK_MAX_BODY_BYTES: usize = 5 * 1024 * 1024;
const WEBHOOK_RATE_PER_SECOND: u64 = 10;
const WEBHOOK_RATE_BURST: u32 = 30;

pub(super) fn webhook_router() -> Router<AppState> {
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(WEBHOOK_RATE_PER_SECOND)
            .burst_size(WEBHOOK_RATE_BURST)
            .finish()
            .expect("webhook governor config must be valid with non-zero burst"),
    );

    Router::new()
        .route("/webhook", post(handle_github_webhook))
        .layer(DefaultBodyLimit::max(WEBHOOK_MAX_BODY_BYTES))
        .layer(GovernorLayer::new(governor_conf))
}

pub(super) fn github_routes() -> Router<AppState> {
    Router::new()
        .merge(webhook_router())
        .route("/auth/login", get(auth_login_handler))
        .route("/auth/callback", get(auth_callback_handler))
        .route("/auth/me", get(auth_me_handler))
}

pub(super) fn gitlab_webhook_router() -> Router<AppState> {
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(WEBHOOK_RATE_PER_SECOND)
            .burst_size(WEBHOOK_RATE_BURST)
            .finish()
            .expect("webhook governor config must be valid with non-zero burst"),
    );

    Router::new()
        .route("/webhook", post(handle_gitlab_webhook))
        .layer(DefaultBodyLimit::max(WEBHOOK_MAX_BODY_BYTES))
        .layer(GovernorLayer::new(governor_conf))
}

pub(super) fn gitlab_routes() -> Router<AppState> {
    Router::new().merge(gitlab_webhook_router())
}

pub(super) fn gitea_webhook_router() -> Router<AppState> {
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .per_second(WEBHOOK_RATE_PER_SECOND)
            .burst_size(WEBHOOK_RATE_BURST)
            .finish()
            .expect("webhook governor config must be valid with non-zero burst"),
    );

    Router::new()
        .route("/webhook", post(handle_gitea_webhook))
        .layer(DefaultBodyLimit::max(WEBHOOK_MAX_BODY_BYTES))
        .layer(GovernorLayer::new(governor_conf))
}

pub(super) fn gitea_routes() -> Router<AppState> {
    Router::new().merge(gitea_webhook_router())
}

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/logs/{drv}", get(get_derivation_log))
        .route("/metrics", get(metrics_handler))
        .route("/commits/{sha}/check_runs", get(get_check_runs_for_commit))
        .route("/ws/builds", get(websocket_handler))
        .route("/repositories", get(list_repositories_handler))
        .route("/repositories/{owner}/{repo}", get(get_repository_handler))
        .route(
            "/repositories/{owner}/{repo}/commits",
            get(list_repository_commits_handler),
        )
        .route(
            "/repositories/{owner}/{repo}/jobsets",
            get(get_repository_jobsets_handler),
        )
        .route("/prs", get(list_pull_requests_handler))
        .route(
            "/prs/{owner}/{repo}/{pr_number}",
            get(get_pull_request_handler),
        )
        .route(
            "/prs/{owner}/{repo}/{pr_number}/github-metadata",
            get(get_pr_github_metadata_handler),
        )
        .route(
            "/prs/{owner}/{repo}/{pr_number}/enable-auto-merge",
            post(enable_auto_merge_handler),
        )
        .route(
            "/prs/{owner}/{repo}/{pr_number}/disable-auto-merge",
            post(disable_auto_merge_handler),
        )
        .route(
            "/prs/{owner}/{repo}/{pr_number}/merge",
            post(manual_merge_pr_handler),
        )
        .route(
            "/prs/{owner}/{repo}/{pr_number}/merge-eligibility",
            get(check_merge_eligibility_handler),
        )
        .route(
            "/merge-queue/{owner}/{repo}",
            get(list_merge_queue_builds_handler),
        )
        .route(
            "/merge-queue/{owner}/{repo}/{sha}",
            get(get_merge_queue_build_handler),
        )
        .route("/commits/{sha}/jobs", get(get_commit_jobs_handler))
        .route(
            "/commits/{sha}/package-changes",
            get(get_package_changes_handler),
        )
        .route(
            "/commits/{sha}/rebuild-impact",
            get(get_rebuild_impact_handler),
        )
        .route(
            "/commits/{sha}/change-summary",
            get(get_change_summary_handler),
        )
        .route(
            "/commits/{sha}/change-summary.md",
            get(get_change_summary_markdown_handler),
        )
        .route("/jobs/{jobset_id}", get(get_jobset_details_handler))
        .route("/jobs/{jobset_id}/drvs", get(get_jobset_drvs_handler))
        .route("/builds/active", get(get_active_builds_handler))
        .route("/drvs/{drv}", get(get_drv_details_handler))
        .route(
            "/drvs/{drv}/dependencies",
            get(get_drv_dependencies_handler),
        )
        .route("/drvs/{drv}/hooks", get(get_drv_hooks_handler))
        .route("/logs/{drv}/hooks/{hook_name}", get(get_hook_log))
        .route("/users/me/profile", get(user_profile_handler))
        .route(
            "/users/me/profile",
            axum::routing::patch(update_user_profile_handler),
        )
        .route(
            "/users/me/maintained-paths",
            get(user_maintained_paths_handler),
        )
        .route("/admin/approved-users", get(list_approved_users_handler))
        .route("/admin/approved-users", post(add_approved_user_handler))
        .route(
            "/admin/approved-users/{username}",
            axum::routing::delete(remove_approved_user_handler),
        )
        .route("/admin/users", get(admin_list_users_handler))
        .route(
            "/admin/users/{github_id}/promote",
            post(admin_promote_user_handler),
        )
        .route(
            "/admin/users/{github_id}/demote",
            post(admin_demote_user_handler),
        )
        .route(
            "/admin/users/{github_id}",
            axum::routing::delete(admin_delete_user_handler),
        )
        .route(
            "/admin/users/{github_id}/maintained-paths",
            get(admin_user_maintained_paths_handler),
        )
        .route(
            "/admin/attr-paths/{attr_path}/maintainers",
            post(admin_add_maintainer_handler),
        )
        .route(
            "/admin/attr-paths/{attr_path}/maintainers/by-username",
            post(admin_add_maintainer_by_username_handler),
        )
        .route(
            "/admin/attr-paths/{attr_path}/maintainers/{github_id}",
            axum::routing::delete(admin_remove_maintainer_handler),
        )
        .route(
            "/admin/attr-paths/{attr_path}/maintainers",
            get(admin_list_maintainers_handler),
        )
        .route(
            "/admin/maintainer-requests",
            get(admin_list_pending_requests_handler),
        )
        .route(
            "/admin/maintainer-requests/{request_id}/approve",
            post(admin_approve_request_handler),
        )
        .route(
            "/admin/maintainer-requests/{request_id}/reject",
            post(admin_reject_request_handler),
        )
        .route(
            "/attr-paths/{attr_path}/maintainers",
            get(get_attr_path_maintainers_handler),
        )
        .route(
            "/jobs/{job_id}/maintainers",
            get(get_job_maintainers_handler),
        )
        .route(
            "/attr-paths/{attr_path}/request-maintainer",
            post(request_maintainer_handler),
        )
        .route(
            "/jobs/{job_id}/request-maintainer",
            post(request_maintainer_for_job_handler),
        )
        .route(
            "/users/me/maintainer-requests",
            get(get_my_requests_handler),
        )
        .route(
            "/maintainer-requests/{request_id}",
            get(get_maintainer_request_handler),
        )
        .route("/drvs/{drv}/rebuild", post(rebuild_drv_handler))
        .route(
            "/admin/rebuild-all-failed",
            post(admin_rebuild_all_failed_handler),
        )
        .route(
            "/admin/github-api-cache/clear",
            post(admin_github_api_cache_clear_handler),
        )
        .route(
            "/admin/github-api-cache/invalidate",
            post(admin_github_api_cache_invalidate_handler),
        )
}
