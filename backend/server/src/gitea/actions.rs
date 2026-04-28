///! Gitea API action wrappers for check run and commit status operations.
///!
///! This module provides high-level functions for interacting with the Gitea API
///! to create and update check runs (newer Gitea instances) or commit statuses
///! (older instances). It automatically handles version detection and fallback.
use anyhow::{Context, Result};
use tracing::debug;

use crate::db::model::build_event::DrvBuildState;
use crate::gitea::client::{
    CheckConclusion, CheckOutput, CheckStatus, CreateCheckRunRequest, GiteaClient,
    UpdateCheckRunRequest,
};
use crate::gitea::types::GiteaCIInfo;

/// Create an initial CI configure gate check run.
///
/// This creates a "queued" check run to indicate that EkaCI is processing
/// the repository configuration.
pub async fn create_ci_configure_gate(client: &GiteaClient, ci_info: &GiteaCIInfo) -> Result<i64> {
    debug!(
        "Creating CI configure gate check run for commit {} in {}/{}",
        &ci_info.commit, ci_info.owner, ci_info.repo_name
    );

    let request = CreateCheckRunRequest {
        name: "EkaCI: Configure".to_string(),
        head_sha: ci_info.commit.clone(),
        status: Some(CheckStatus::Queued),
        conclusion: None,
        output: None,
    };

    let check_run = client
        .create_check_run(&ci_info.owner, &ci_info.repo_name, request)
        .await
        .context("Failed to create CI configure gate check run")?;

    debug!(
        "Successfully created CI configure gate check run {} for commit {}",
        check_run.id, &ci_info.commit
    );

    Ok(check_run.id)
}

/// Update the CI configure gate check run to success.
///
/// Marks the configuration processing as complete and successful.
pub async fn update_ci_configure_gate(
    client: &GiteaClient,
    ci_info: &GiteaCIInfo,
    check_run_id: i64,
) -> Result<()> {
    debug!(
        "Updating CI configure gate check run {} to success for commit {} in {}/{}",
        check_run_id, &ci_info.commit, ci_info.owner, ci_info.repo_name
    );

    let request = UpdateCheckRunRequest {
        status: Some(CheckStatus::Completed),
        conclusion: Some(CheckConclusion::Success),
        output: None,
    };

    client
        .update_check_run(&ci_info.owner, &ci_info.repo_name, check_run_id, request)
        .await
        .context("Failed to update CI configure gate check run")?;

    debug!(
        "Successfully updated CI configure gate check run {} for commit {}",
        check_run_id, &ci_info.commit
    );

    Ok(())
}

/// Create a CI eval job check run.
///
/// This creates a "running" check run to indicate that job evaluation is in progress.
pub async fn create_ci_eval_job(
    client: &GiteaClient,
    ci_info: &GiteaCIInfo,
    job_title: &str,
) -> Result<i64> {
    debug!(
        "Creating CI eval job check run for job '{}' on commit {} in {}/{}",
        job_title, &ci_info.commit, ci_info.owner, ci_info.repo_name
    );

    let name = format!("EkaCI: Evaluate Job ({})", job_title);

    let request = CreateCheckRunRequest {
        name,
        head_sha: ci_info.commit.clone(),
        status: Some(CheckStatus::InProgress),
        conclusion: None,
        output: None,
    };

    let check_run = client
        .create_check_run(&ci_info.owner, &ci_info.repo_name, request)
        .await
        .context("Failed to create CI eval job check run")?;

    debug!(
        "Successfully created CI eval job check run {} for job '{}'",
        check_run.id, job_title
    );

    Ok(check_run.id)
}

/// Update a CI eval job check run to completion.
///
/// Marks the evaluation as complete with either success or failure.
pub async fn update_ci_eval_job(
    client: &GiteaClient,
    ci_info: &GiteaCIInfo,
    job_name: &str,
    check_run_id: i64,
    success: bool,
) -> Result<()> {
    debug!(
        "Updating CI eval job check run {} for job '{}' to {} on commit {} in {}/{}",
        check_run_id,
        job_name,
        if success { "success" } else { "failure" },
        &ci_info.commit,
        ci_info.owner,
        ci_info.repo_name
    );

    let conclusion = if success {
        CheckConclusion::Success
    } else {
        CheckConclusion::Failure
    };

    let request = UpdateCheckRunRequest {
        status: Some(CheckStatus::Completed),
        conclusion: Some(conclusion),
        output: None,
    };

    client
        .update_check_run(&ci_info.owner, &ci_info.repo_name, check_run_id, request)
        .await
        .context("Failed to update CI eval job check run")?;

    debug!(
        "Successfully updated CI eval job check run {} for job '{}'",
        check_run_id, job_name
    );

    Ok(())
}

/// Create a failed CI eval job check run with error details.
///
/// Creates a "failure" check run with a summary of evaluation errors.
pub async fn fail_ci_eval_job(
    client: &GiteaClient,
    ci_info: &GiteaCIInfo,
    job_name: &str,
    errors: &[crate::nix::nix_eval_jobs::NixEvalError],
) -> Result<i64> {
    debug!(
        "Creating failed CI eval job check run for job '{}' on commit {} with {} errors",
        job_name,
        &ci_info.commit,
        errors.len()
    );

    let name = format!("EkaCI: Evaluate Job ({})", job_name);

    // Create a summary of errors
    // TODO: Could enhance this with detailed output
    let summary = if errors.len() == 1 {
        format!("Evaluation failed: {}", &errors[0].attr)
    } else {
        format!("Evaluation failed with {} errors", errors.len())
    };

    let request = CreateCheckRunRequest {
        name,
        head_sha: ci_info.commit.clone(),
        status: Some(CheckStatus::Completed),
        conclusion: Some(CheckConclusion::Failure),
        output: Some(crate::gitea::client::CheckOutput {
            title: "Evaluation Failed".to_string(),
            summary,
            text: None,
        }),
    };

    let check_run = client
        .create_check_run(&ci_info.owner, &ci_info.repo_name, request)
        .await
        .context("Failed to create failed CI eval job check run")?;

    debug!(
        "Successfully created failed CI eval job check run {} for job '{}'",
        check_run.id, job_name
    );

    Ok(check_run.id)
}

/// Create a check run for a build.
///
/// This creates a new check run with the given name and status.
#[allow(dead_code)]
pub async fn create_check_run(
    client: &GiteaClient,
    owner: &str,
    repo: &str,
    sha: &str,
    name: &str,
    status: CheckStatus,
    conclusion: Option<CheckConclusion>,
) -> Result<i64> {
    debug!(
        "Creating check run '{}' with status {:?} for commit {} in {}/{}",
        name, status, sha, owner, repo
    );

    let request = CreateCheckRunRequest {
        name: name.to_string(),
        head_sha: sha.to_string(),
        status: Some(status),
        conclusion,
        output: None,
    };

    let check_run = client
        .create_check_run(owner, repo, request)
        .await
        .with_context(|| format!("Failed to create check run '{}'", name))?;

    debug!(
        "Successfully created check run {} for commit {}",
        check_run.id, sha
    );

    Ok(check_run.id)
}

/// Update a check run to a new status.
///
/// Updates an existing check run with new status and optional conclusion.
pub async fn update_check_run(
    client: &GiteaClient,
    owner: &str,
    repo: &str,
    check_run_id: i64,
    status: CheckStatus,
    conclusion: Option<CheckConclusion>,
) -> Result<()> {
    debug!(
        "Updating check run {} to status {:?} in {}/{}",
        check_run_id, status, owner, repo
    );

    let request = UpdateCheckRunRequest {
        status: Some(status),
        conclusion,
        output: None,
    };

    client
        .update_check_run(owner, repo, check_run_id, request)
        .await
        .with_context(|| format!("Failed to update check run {}", check_run_id))?;

    debug!("Successfully updated check run {} ", check_run_id);

    Ok(())
}

/// Create a failure check run for a build.
///
/// This is used when a build fails - we create a "failure" check run with details.
pub async fn create_failure_check_run(
    client: &GiteaClient,
    owner: &str,
    repo: &str,
    sha: &str,
    name: &str,
    description: &str,
) -> Result<i64> {
    debug!(
        "Creating failure check run '{}' for commit {} in {}/{}",
        name, sha, owner, repo
    );

    let request = CreateCheckRunRequest {
        name: name.to_string(),
        head_sha: sha.to_string(),
        status: Some(CheckStatus::Completed),
        conclusion: Some(CheckConclusion::Failure),
        output: Some(crate::gitea::client::CheckOutput {
            title: "Build Failed".to_string(),
            summary: description.to_string(),
            text: None,
        }),
    };

    let check_run = client
        .create_check_run(owner, repo, request)
        .await
        .with_context(|| format!("Failed to create failure check run '{}'", name))?;

    debug!(
        "Successfully created failure check run {} for commit {}",
        check_run.id, sha
    );

    Ok(check_run.id)
}

/// Cancel a check run.
///
/// Sets the check run to "cancelled" conclusion, indicating the build was interrupted.
pub async fn cancel_check_run(
    client: &GiteaClient,
    owner: &str,
    repo: &str,
    check_run_id: i64,
) -> Result<()> {
    debug!("Canceling check run {} in {}/{}", check_run_id, owner, repo);

    let request = UpdateCheckRunRequest {
        status: Some(CheckStatus::Completed),
        conclusion: Some(CheckConclusion::Cancelled),
        output: None,
    };

    client
        .update_check_run(owner, repo, check_run_id, request)
        .await
        .context("Failed to cancel check run")?;

    debug!("Successfully canceled check run {}", check_run_id);

    Ok(())
}

/// Update a build check run based on DrvBuildState.
///
/// This is a convenience function that converts a DrvBuildState to the appropriate
/// Gitea check run status and conclusion, then updates the check run.
pub async fn update_build_check_run(
    client: &GiteaClient,
    owner: &str,
    repo: &str,
    check_run_id: i64,
    build_state: &DrvBuildState,
) -> Result<()> {
    let (status, conclusion) = status_from_build_state(build_state);

    update_check_run(client, owner, repo, check_run_id, status, conclusion).await
}

/// Convert DrvBuildState to Gitea CheckStatus and CheckConclusion
fn status_from_build_state(state: &DrvBuildState) -> (CheckStatus, Option<CheckConclusion>) {
    use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult};

    match state {
        DrvBuildState::Queued | DrvBuildState::Buildable | DrvBuildState::Blocked => {
            (CheckStatus::Queued, None)
        },
        DrvBuildState::FailedRetry => (CheckStatus::Queued, None), // Will retry
        DrvBuildState::Building => (CheckStatus::InProgress, None),
        DrvBuildState::Completed(DrvBuildResult::Success) => {
            (CheckStatus::Completed, Some(CheckConclusion::Success))
        },
        DrvBuildState::Completed(DrvBuildResult::Failure) => {
            (CheckStatus::Completed, Some(CheckConclusion::Failure))
        },
        DrvBuildState::TransitiveFailure => {
            (CheckStatus::Completed, Some(CheckConclusion::Failure))
        },
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => {
            (CheckStatus::Completed, Some(CheckConclusion::Cancelled))
        },
        DrvBuildState::Interrupted(_) => (CheckStatus::Completed, Some(CheckConclusion::Failure)),
        DrvBuildState::UnsatisfiableRequirements => {
            (CheckStatus::Completed, Some(CheckConclusion::Failure))
        },
    }
}

// ============================================================================
// Auto-merge helper functions
// ============================================================================

/// Result of merge method validation
#[derive(Debug)]
pub enum MergeMethodCheck {
    Ok,
    NotAllowed { allowed: Vec<String> },
}

/// Repository permission information
#[derive(Debug)]
pub struct RepoPermission {
    pub can_push: bool,
    pub is_admin: bool,
}

/// Validate a merge method against repository settings
pub async fn validate_merge_method(
    _client: &GiteaClient,
    owner: &str,
    repo_name: &str,
    method: &str,
) -> Result<MergeMethodCheck> {
    debug!(
        "Validating merge method '{}' for {}/{}",
        method, owner, repo_name
    );

    // Gitea supports three merge methods (similar to GitHub):
    // - "merge" - creates a merge commit
    // - "rebase" - rebases and merges
    // - "squash" - squashes commits and merges
    //
    // For now, accept all standard methods since Gitea doesn't have
    // strict repository-level restrictions on merge methods.
    // Individual repos may have preferences, but those are not enforced via API.

    match method {
        "merge" | "rebase" | "squash" => Ok(MergeMethodCheck::Ok),
        _ => {
            // Unknown method - return allowed methods
            Ok(MergeMethodCheck::NotAllowed {
                allowed: vec![
                    "merge".to_string(),
                    "rebase".to_string(),
                    "squash".to_string(),
                ],
            })
        },
    }
}

/// Check repository permission for a user
pub async fn check_repo_permission_for_user(
    client: &GiteaClient,
    owner: &str,
    repo_name: &str,
    username: &str,
) -> Result<RepoPermission> {
    debug!(
        "Checking repo permission for {} in {}/{}",
        username, owner, repo_name
    );

    // Use the check_user_permission method if available
    match client
        .check_user_permission(owner, repo_name, username)
        .await
    {
        Ok(permission) => {
            // Parse permission string: "admin", "write", "read", "none"
            let can_push = matches!(permission.permission.as_str(), "admin" | "write");
            let is_admin = permission.permission == "admin";

            debug!(
                "User {} has permission '{}' (push={}, admin={}) in {}/{}",
                username, permission.permission, can_push, is_admin, owner, repo_name
            );
            Ok(RepoPermission { can_push, is_admin })
        },
        Err(e) => {
            debug!(
                "Failed to check repo permission for {} in {}/{}: {}",
                username, owner, repo_name, e
            );
            // Return no permissions if we can't fetch
            Ok(RepoPermission {
                can_push: false,
                is_admin: false,
            })
        },
    }
}

/// Check if all changed packages have required approvals
pub async fn check_pr_maintainer_approvals(
    _client: &GiteaClient,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    _changed_packages: &[String],
    _pool: &sqlx::Pool<sqlx::Sqlite>,
) -> Result<(bool, Vec<String>)> {
    debug!(
        "Checking maintainer approvals for PR #{} in {}/{}",
        pr_number, owner, repo_name
    );

    // For now, consider all packages approved
    // Real implementation would check package maintainers and PR approvals
    Ok((true, vec![]))
}

/// Fetch the commit date for a commit SHA
pub async fn fetch_head_commit_date(
    client: &GiteaClient,
    owner: &str,
    repo_name: &str,
    sha: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    debug!(
        "Fetching commit date for {} in {}/{}",
        sha, owner, repo_name
    );

    match client.get_commit(owner, repo_name, sha).await {
        Ok(commit) => {
            // Parse the committer date field (ISO 8601 format)
            match chrono::DateTime::parse_from_rfc3339(&commit.commit.committer.date) {
                Ok(dt) => {
                    let utc_dt = dt.with_timezone(&chrono::Utc);
                    debug!("Commit {} was committed at {}", sha, utc_dt);
                    Ok(Some(utc_dt))
                },
                Err(e) => {
                    debug!(
                        "Failed to parse commit date '{}' for {}: {}",
                        commit.commit.committer.date, sha, e
                    );
                    Ok(None)
                },
            }
        },
        Err(e) => {
            debug!(
                "Failed to fetch commit {} in {}/{}: {}",
                sha, owner, repo_name, e
            );
            Ok(None)
        },
    }
}

/// Create a dependency changes gate check run
pub async fn create_dependency_changes_gate(
    client: &GiteaClient,
    ci_info: &crate::gitea::types::GiteaCIInfo,
    dependency_diff: &str,
    num_packages: usize,
) -> Result<()> {
    debug!(
        "Creating dependency changes gate for commit {} ({} packages)",
        &ci_info.commit, num_packages
    );

    let name = "EkaCI: Dependency Changes";

    let summary = if num_packages == 0 {
        "No runtime dependency changes detected".to_string()
    } else {
        format!(
            "{} package(s) have changed runtime dependencies",
            num_packages
        )
    };

    let text = if dependency_diff.len() > 1000 {
        format!("{}\n\n(truncated)", &dependency_diff[..1000])
    } else {
        dependency_diff.to_string()
    };

    let request = CreateCheckRunRequest {
        name: name.to_string(),
        head_sha: ci_info.commit.clone(),
        status: Some(CheckStatus::Completed),
        conclusion: Some(CheckConclusion::Success),
        output: Some(CheckOutput {
            title: "Dependency Changes".to_string(),
            summary,
            text: if text.is_empty() { None } else { Some(text) },
        }),
    };

    client
        .create_check_run(&ci_info.owner, &ci_info.repo_name, request)
        .await
        .context("Failed to create dependency changes gate")?;

    Ok(())
}
