///! GitLab API action wrappers for commit status operations.
///!
///! This module provides high-level functions for interacting with the GitLab API
///! to create and update commit statuses. It follows the same pattern as the GitHub
///! actions module but uses GitLab's Commit Status API instead of Check Runs.
use anyhow::{Context, Result};
use tracing::debug;

use crate::db::model::build_event::DrvBuildState;
use crate::gitlab::client::{CommitStatusState, CreateCommitStatusRequest, GitLabClient};
use crate::gitlab::types::GitLabCIInfo;

/// Create an initial CI configure gate status.
///
/// This creates a "pending" commit status to indicate that EkaCI is processing
/// the repository configuration.
pub async fn create_ci_configure_gate(
    client: &GitLabClient,
    ci_info: &GitLabCIInfo,
) -> Result<i64> {
    debug!(
        "Creating CI configure gate status for commit {} in project {}",
        &ci_info.commit, ci_info.project_id
    );

    let request = CreateCommitStatusRequest {
        state: CommitStatusState::Pending,
        target_url: None,
        description: Some("Processing CI configuration".to_string()),
        name: Some("EkaCI: Configure".to_string()),
        context: Some("ekaci/configure".to_string()),
    };

    let status = client
        .create_commit_status(ci_info.project_id, &ci_info.commit, request)
        .await
        .context("Failed to create CI configure gate status")?;

    debug!(
        "Successfully created CI configure gate status {} for commit {}",
        status.id, &ci_info.commit
    );

    Ok(status.id)
}

/// Update the CI configure gate status to success.
///
/// Marks the configuration processing as complete and successful.
pub async fn update_ci_configure_gate(client: &GitLabClient, ci_info: &GitLabCIInfo) -> Result<()> {
    debug!(
        "Updating CI configure gate status to success for commit {} in project {}",
        &ci_info.commit, ci_info.project_id
    );

    let request = CreateCommitStatusRequest {
        state: CommitStatusState::Success,
        target_url: None,
        description: Some("CI configuration validated successfully".to_string()),
        name: Some("EkaCI: Configure".to_string()),
        context: Some("ekaci/configure".to_string()),
    };

    client
        .create_commit_status(ci_info.project_id, &ci_info.commit, request)
        .await
        .context("Failed to update CI configure gate status")?;

    debug!(
        "Successfully updated CI configure gate status for commit {}",
        &ci_info.commit
    );

    Ok(())
}

/// Create a CI eval job status.
///
/// This creates a "running" commit status to indicate that job evaluation is in progress.
pub async fn create_ci_eval_job(
    client: &GitLabClient,
    ci_info: &GitLabCIInfo,
    job_title: &str,
) -> Result<i64> {
    debug!(
        "Creating CI eval job status for job '{}' on commit {} in project {}",
        job_title, &ci_info.commit, ci_info.project_id
    );

    let context = format!("ekaci/eval-{}", job_title);
    let name = format!("EkaCI: Evaluate Job ({})", job_title);

    let request = CreateCommitStatusRequest {
        state: CommitStatusState::Running,
        target_url: None,
        description: Some("Evaluating Nix expressions".to_string()),
        name: Some(name),
        context: Some(context),
    };

    let status = client
        .create_commit_status(ci_info.project_id, &ci_info.commit, request)
        .await
        .context("Failed to create CI eval job status")?;

    debug!(
        "Successfully created CI eval job status {} for job '{}'",
        status.id, job_title
    );

    Ok(status.id)
}

/// Update a CI eval job status to completion.
///
/// Marks the evaluation as complete with either success or failure.
pub async fn update_ci_eval_job(
    client: &GitLabClient,
    ci_info: &GitLabCIInfo,
    job_name: &str,
    success: bool,
) -> Result<()> {
    debug!(
        "Updating CI eval job status for job '{}' to {} on commit {} in project {}",
        job_name,
        if success { "success" } else { "failed" },
        &ci_info.commit,
        ci_info.project_id
    );

    let context = format!("ekaci/eval-{}", job_name);
    let name = format!("EkaCI: Evaluate Job ({})", job_name);
    let state = if success {
        CommitStatusState::Success
    } else {
        CommitStatusState::Failed
    };
    let description = if success {
        "Evaluation completed successfully"
    } else {
        "Evaluation failed"
    };

    let request = CreateCommitStatusRequest {
        state,
        target_url: None,
        description: Some(description.to_string()),
        name: Some(name),
        context: Some(context),
    };

    client
        .create_commit_status(ci_info.project_id, &ci_info.commit, request)
        .await
        .context("Failed to update CI eval job status")?;

    debug!(
        "Successfully updated CI eval job status for job '{}'",
        job_name
    );

    Ok(())
}

/// Create a failed CI eval job status with error details.
///
/// Creates a "failed" commit status with a summary of evaluation errors.
/// GitLab commit statuses have limited space, so we keep the description concise.
pub async fn fail_ci_eval_job(
    client: &GitLabClient,
    ci_info: &GitLabCIInfo,
    job_name: &str,
    errors: &[crate::nix::nix_eval_jobs::NixEvalError],
) -> Result<i64> {
    debug!(
        "Creating failed CI eval job status for job '{}' on commit {} with {} errors",
        job_name,
        &ci_info.commit,
        errors.len()
    );

    let context = format!("ekaci/eval-{}", job_name);
    let name = format!("EkaCI: Evaluate Job ({})", job_name);

    // Create a concise description with error count
    // Full error details would need to be posted as an MR comment (future work)
    let description = if errors.len() == 1 {
        format!("Evaluation failed: {}", &errors[0].attr)
    } else {
        format!("Evaluation failed with {} errors", errors.len())
    };

    let request = CreateCommitStatusRequest {
        state: CommitStatusState::Failed,
        target_url: None,
        description: Some(description),
        name: Some(name),
        context: Some(context),
    };

    let status = client
        .create_commit_status(ci_info.project_id, &ci_info.commit, request)
        .await
        .context("Failed to create failed CI eval job status")?;

    debug!(
        "Successfully created failed CI eval job status {} for job '{}'",
        status.id, job_name
    );

    Ok(status.id)
}

/// Create a commit status for a build.
///
/// This creates a new commit status with the given name and state.
pub async fn create_commit_status(
    client: &GitLabClient,
    project_id: i64,
    sha: &str,
    context: &str,
    state: CommitStatusState,
    description: Option<&str>,
) -> Result<i64> {
    debug!(
        "Creating commit status '{}' with state {:?} for commit {} in project {}",
        context, state, sha, project_id
    );

    let request = CreateCommitStatusRequest {
        state,
        target_url: None,
        description: description.map(|s| s.to_string()),
        name: Some(context.to_string()),
        context: Some(context.to_string()),
    };

    let status = client
        .create_commit_status(project_id, sha, request)
        .await
        .with_context(|| format!("Failed to create commit status '{}'", context))?;

    debug!(
        "Successfully created commit status {} for commit {}",
        status.id, sha
    );

    Ok(status.id)
}

/// Update a commit status to a new state.
///
/// GitLab doesn't have a direct "update" API for statuses - instead, we create
/// a new status with the same context to replace the old one.
pub async fn update_commit_status(
    client: &GitLabClient,
    project_id: i64,
    sha: &str,
    context: &str,
    state: CommitStatusState,
    description: Option<&str>,
) -> Result<()> {
    debug!(
        "Updating commit status '{}' to state {:?} for commit {} in project {}",
        context, state, sha, project_id
    );

    let request = CreateCommitStatusRequest {
        state,
        target_url: None,
        description: description.map(|s| s.to_string()),
        name: Some(context.to_string()),
        context: Some(context.to_string()),
    };

    client
        .create_commit_status(project_id, sha, request)
        .await
        .with_context(|| format!("Failed to update commit status '{}'", context))?;

    debug!(
        "Successfully updated commit status '{}' for commit {}",
        context, sha
    );

    Ok(())
}

/// Create a failure commit status for a build.
///
/// This is used when a build fails - we create a "failed" status with details.
pub async fn create_failure_status(
    client: &GitLabClient,
    project_id: i64,
    sha: &str,
    context: &str,
    description: &str,
) -> Result<i64> {
    debug!(
        "Creating failure status '{}' for commit {} in project {}",
        context, sha, project_id
    );

    let request = CreateCommitStatusRequest {
        state: CommitStatusState::Failed,
        target_url: None,
        description: Some(description.to_string()),
        name: Some(context.to_string()),
        context: Some(context.to_string()),
    };

    let status = client
        .create_commit_status(project_id, sha, request)
        .await
        .with_context(|| format!("Failed to create failure status '{}'", context))?;

    debug!(
        "Successfully created failure status {} for commit {}",
        status.id, sha
    );

    Ok(status.id)
}

/// Update a commit status with size warning details.
///
/// When a build succeeds but the output size significantly increased, we create
/// a warning status. GitLab doesn't have a "warning" state, so we use "success"
/// with a descriptive message.
pub async fn update_status_with_size_warning(
    client: &GitLabClient,
    project_id: i64,
    sha: &str,
    context: &str,
    baseline_size: u64,
    current_size: u64,
    increase_percent: f64,
) -> Result<()> {
    let description = format!(
        "Build succeeded with size warning: {}% increase ({} → {})",
        increase_percent as u64,
        crate::nix::size::format_size(baseline_size),
        crate::nix::size::format_size(current_size)
    );

    debug!(
        "Updating status '{}' with size warning for commit {} in project {}",
        context, sha, project_id
    );

    let request = CreateCommitStatusRequest {
        state: CommitStatusState::Success, // Still success, but with warning in description
        target_url: None,
        description: Some(description),
        name: Some(context.to_string()),
        context: Some(context.to_string()),
    };

    client
        .create_commit_status(project_id, sha, request)
        .await
        .context("Failed to update status with size warning")?;

    debug!(
        "Successfully updated status '{}' with size warning for commit {}",
        context, sha
    );

    Ok(())
}

/// Cancel a commit status.
///
/// Sets the status to "canceled" state, indicating the build was interrupted.
pub async fn cancel_commit_status(
    client: &GitLabClient,
    project_id: i64,
    sha: &str,
    context: &str,
) -> Result<()> {
    debug!(
        "Canceling commit status '{}' for commit {} in project {}",
        context, sha, project_id
    );

    let request = CreateCommitStatusRequest {
        state: CommitStatusState::Canceled,
        target_url: None,
        description: Some("Build canceled".to_string()),
        name: Some(context.to_string()),
        context: Some(context.to_string()),
    };

    client
        .create_commit_status(project_id, sha, request)
        .await
        .context("Failed to cancel commit status")?;

    debug!(
        "Successfully canceled commit status '{}' for commit {}",
        context, sha
    );

    Ok(())
}

/// Update a build status based on DrvBuildState.
///
/// This is a convenience function that converts a DrvBuildState to the appropriate
/// GitLab status state and updates the commit status.
pub async fn update_build_status(
    client: &GitLabClient,
    project_id: i64,
    sha: &str,
    context: &str,
    build_state: &DrvBuildState,
) -> Result<()> {
    let state = state_from_build_state(build_state);
    let description = format!("Build {}", state_description(build_state));

    update_commit_status(client, project_id, sha, context, state, Some(&description)).await
}

/// Convert DrvBuildState to GitLab CommitStatusState
fn state_from_build_state(state: &DrvBuildState) -> CommitStatusState {
    use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult};

    match state {
        DrvBuildState::Queued | DrvBuildState::Buildable => CommitStatusState::Pending,
        DrvBuildState::FailedRetry => CommitStatusState::Pending, // Will retry
        DrvBuildState::Building => CommitStatusState::Running,
        DrvBuildState::Completed(DrvBuildResult::Success) => CommitStatusState::Success,
        DrvBuildState::Completed(DrvBuildResult::Failure) => CommitStatusState::Failed,
        DrvBuildState::TransitiveFailure => CommitStatusState::Failed,
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => {
            CommitStatusState::Canceled
        },
        DrvBuildState::Interrupted(_) => CommitStatusState::Failed,
        DrvBuildState::UnsatisfiableRequirements => CommitStatusState::Failed,
        DrvBuildState::Blocked => CommitStatusState::Pending,
    }
}

/// Get a human-readable description of a build state.
fn state_description(state: &DrvBuildState) -> &'static str {
    use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult};

    match state {
        DrvBuildState::Queued => "queued",
        DrvBuildState::Buildable => "ready to build",
        DrvBuildState::FailedRetry => "retrying after failure",
        DrvBuildState::Building => "in progress",
        DrvBuildState::Completed(DrvBuildResult::Success) => "succeeded",
        DrvBuildState::Completed(DrvBuildResult::Failure) => "failed",
        DrvBuildState::TransitiveFailure => "skipped (dependency failed)",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::OutOfMemory) => {
            "failed (out of memory)"
        },
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout) => "failed (timeout)",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => "canceled",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath) => {
            "failed (process died)"
        },
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::SchedulerDeath) => {
            "failed (scheduler died)"
        },
        DrvBuildState::UnsatisfiableRequirements => "failed (unsatisfiable requirements)",
        DrvBuildState::Blocked => "blocked (waiting for dependencies)",
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

/// Validate a merge method against project settings
pub async fn validate_merge_method(
    client: &GitLabClient,
    project_id: i64,
    method: &str,
) -> Result<MergeMethodCheck> {
    // For now, accept all merge methods - GitLab project settings validation
    // can be added later by fetching project details
    debug!(
        "Validating merge method '{}' for project {}",
        method, project_id
    );
    let _ = (client, method);
    Ok(MergeMethodCheck::Ok)
}

/// Check project permission level for a user
pub async fn check_project_permission_for_user(
    client: &GitLabClient,
    project_id: i64,
    user_id: i64,
) -> Result<i32> {
    // Fetch project member details for the user
    debug!(
        "Checking project permission for user {} in project {}",
        user_id, project_id
    );

    // For now, return a default permission level
    // This should be replaced with actual GitLab API call
    let _ = (client, user_id);
    Ok(0) // 0 = no access, 30 = developer, 40 = maintainer, 50 = owner
}

/// Check if all changed packages have required approvals
pub async fn check_mr_maintainer_approvals(
    client: &GitLabClient,
    project_id: i64,
    mr_iid: i64,
    _changed_packages: &[String],
    pool: &sqlx::Pool<sqlx::Sqlite>,
) -> Result<(bool, Vec<String>)> {
    debug!(
        "Checking maintainer approvals for MR !{} in project {}",
        mr_iid, project_id
    );

    let _ = (client, pool);

    // For now, consider all packages approved
    // Real implementation would check package maintainers and MR approvals
    Ok((true, vec![]))
}

/// Fetch the commit date for a commit SHA
pub async fn fetch_head_commit_date(
    client: &GitLabClient,
    project_id: i64,
    sha: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    debug!("Fetching commit date for {} in project {}", sha, project_id);

    // For now, return None to skip the push timing check
    // Real implementation would call GitLab API to get commit details
    let _ = (client, sha);
    Ok(None)
}

/// Create a dependency changes gate status
pub async fn create_dependency_changes_gate(
    client: &GitLabClient,
    ci_info: &crate::gitlab::types::GitLabCIInfo,
    dependency_diff: &str,
    num_packages: usize,
) -> Result<()> {
    debug!(
        "Creating dependency changes gate for commit {} ({} packages)",
        &ci_info.commit, num_packages
    );

    let context = "ekaci/dependency-changes";
    let name = "EkaCI: Dependency Changes";

    let summary = if num_packages == 0 {
        "No runtime dependency changes detected".to_string()
    } else {
        format!(
            "{} package(s) have changed runtime dependencies",
            num_packages
        )
    };

    let description = if dependency_diff.len() > 200 {
        format!("{}\n\n(truncated)", &dependency_diff[..200])
    } else {
        dependency_diff.to_string()
    };

    let request = CreateCommitStatusRequest {
        state: CommitStatusState::Success,
        target_url: None,
        description: Some(if description.is_empty() {
            summary
        } else {
            format!("{}\n\n{}", summary, description)
        }),
        name: Some(name.to_string()),
        context: Some(context.to_string()),
    };

    client
        .create_commit_status(ci_info.project_id, &ci_info.commit, request)
        .await
        .context("Failed to create dependency changes gate")?;

    Ok(())
}
