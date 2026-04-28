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
