// Pull request and merge-related handlers

use axum::extract::{Json, Path, State};
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use super::responses::{bad_request, forbidden, internal_error, not_found, service_unavailable};
use super::state::AppState;
use crate::auth::AuthUser;

pub(super) async fn list_pull_requests_handler(State(state): State<AppState>) -> impl IntoResponse {
    match state.db_service.list_open_pull_requests().await {
        Ok(prs) => Json(prs).into_response(),
        Err(e) => {
            error!("Failed to list pull requests: {}", e);
            internal_error(format!("Failed to list pull requests: {}", e))
        },
    }
}

pub(super) async fn get_pull_request_handler(
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

pub(super) async fn get_pr_github_metadata_handler(
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

/// Ensure the authenticated user is a maintainer of every package changed by
/// the referenced PR. Returns the changed package list on success, or a ready
/// `Response` describing the failure that the caller should return directly.
async fn ensure_pr_maintainer_access(
    auth_user: &AuthUser,
    owner: &str,
    repo: &str,
    pr_number: i64,
    pool: &sqlx::Pool<sqlx::Sqlite>,
    action_verb_phrase: &str,
) -> Result<Vec<String>, axum::response::Response> {
    let changed_packages = crate::db::github::get_pr_changed_packages(pr_number, owner, repo, pool)
        .await
        .map_err(|e| {
            error!("Failed to get changed packages for PR: {}", e);
            internal_error(format!("Failed to get changed packages: {}", e))
        })?;

    if changed_packages.is_empty() {
        return Err(bad_request("No changed packages found for this PR"));
    }

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

#[derive(Debug, Deserialize)]
pub(super) struct EnableAutoMergeRequest {
    merge_method: Option<String>,
}

pub(super) async fn enable_auto_merge_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
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

    if let Some(ref method) = payload.merge_method {
        if method != "merge" && method != "squash" && method != "rebase" {
            return bad_request("Invalid merge method. Must be 'merge', 'squash', or 'rebase'");
        }
    }

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

pub(super) async fn disable_auto_merge_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
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

#[derive(Debug, Deserialize)]
pub(super) struct ManualMergeRequest {
    merge_method: Option<String>,
}

pub(super) async fn manual_merge_pr_handler(
    State(state): State<AppState>,
    auth_user: AuthUser,
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

    let merge_method = payload
        .merge_method
        .as_deref()
        .or(Some(state.default_merge_method.as_str()))
        .unwrap_or("squash");

    if merge_method != "merge" && merge_method != "squash" && merge_method != "rebase" {
        return bad_request("Invalid merge method. Must be 'merge', 'squash', or 'rebase'");
    }

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

            let user_id = sqlx::query_scalar::<_, i64>(
                "SELECT ROWID FROM AuthenticatedUsers WHERE github_id = ?",
            )
            .bind(auth_user.github_id)
            .fetch_optional(&state.db_service.pool)
            .await
            .ok()
            .flatten();

            if let Err(e) = crate::db::github::mark_pr_merged(
                &owner,
                &repo,
                pr_number,
                user_id,
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

#[derive(Debug, Serialize)]
struct MergeEligibility {
    eligible: bool,
    auto_merge_enabled: bool,
    gates_passed: bool,
    has_maintainer_approvals: bool,
    changed_packages: Vec<String>,
    missing_approvals: std::collections::HashMap<String, Vec<String>>,
}

pub(super) async fn check_merge_eligibility_handler(
    State(state): State<AppState>,
    Path((owner, repo, pr_number)): Path<(String, String, i64)>,
) -> impl IntoResponse {
    let octocrab = match &state.octocrab {
        Some(o) => o,
        None => return service_unavailable("GitHub API not available"),
    };

    let pr = match sqlx::query_as::<_, crate::db::github::PullRequest>(
        "SELECT * FROM GitHubPullRequests WHERE owner = ? AND repo_name = ? AND pr_number = ?",
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

pub(super) async fn list_merge_queue_builds_handler(
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

pub(super) async fn get_merge_queue_build_handler(
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
