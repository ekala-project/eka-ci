// GitLab commit status operations

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::gitlab::GitLabClient;
use crate::gitlab::types::GitLabCIInfo;

/// Create a CI configure gate commit status
pub(super) async fn handle_create_ci_configure_gate(
    ci_info: &Arc<GitLabCIInfo>,
    client: &GitLabClient,
    configure_statuses: &Mutex<HashMap<String, i64>>,
) -> Result<()> {
    debug!(
        "Creating CI configure gate status for commit {} in project {}",
        ci_info.commit, ci_info.project_id
    );

    match crate::gitlab::actions::create_ci_configure_gate(client, ci_info).await {
        Ok(status_id) => {
            // Store the status ID for later updates
            configure_statuses
                .lock()
                .await
                .insert(ci_info.commit.clone(), status_id);
            info!(
                "Created CI configure gate status {} for commit {}",
                status_id, ci_info.commit
            );
        },
        Err(e) => {
            warn!(
                "Failed to create CI configure gate status for commit {}: {:?}",
                ci_info.commit, e
            );
        },
    }

    Ok(())
}

/// Complete the CI configure gate commit status
pub(super) async fn handle_complete_ci_configure_gate(
    ci_info: &Arc<GitLabCIInfo>,
    client: &GitLabClient,
    configure_statuses: &Mutex<HashMap<String, i64>>,
) -> Result<()> {
    debug!(
        "Completing CI configure gate status for commit {} in project {}",
        ci_info.commit, ci_info.project_id
    );

    // Remove from tracking map
    configure_statuses.lock().await.remove(&ci_info.commit);

    match crate::gitlab::actions::update_ci_configure_gate(client, ci_info).await {
        Ok(()) => {
            info!(
                "Successfully completed CI configure gate status for commit {}",
                ci_info.commit
            );
        },
        Err(e) => {
            warn!(
                "Failed to complete CI configure gate status for commit {}: {:?}",
                ci_info.commit, e
            );
        },
    }

    Ok(())
}

/// Update the status of a build in GitLab
pub(super) async fn handle_update_build_status(
    drv_id: &crate::db::model::DrvId,
    status: &crate::db::model::build_event::DrvBuildState,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    clients: &HashMap<String, Arc<GitLabClient>>,
) -> Result<()> {
    debug!(
        "Updating GitLab build status for {:?}: {:?}",
        drv_id, status
    );

    // Query all commit statuses associated with this derivation
    let statuses = crate::db::gitlab::commit_statuses_for_drv_path(drv_id, db_pool).await?;

    if statuses.is_empty() {
        debug!("No commit statuses found for drv {:?}", drv_id);
        return Ok(());
    }

    // Update each status
    for commit_status in statuses {
        let Some(client) = clients.get(&commit_status.domain) else {
            warn!("No GitLab client for domain {}", commit_status.domain);
            continue;
        };

        // Use the actions module to update the status
        if let Err(e) = crate::gitlab::actions::update_build_status(
            client,
            commit_status.project_id,
            &commit_status.sha,
            &commit_status.name,
            status,
        )
        .await
        {
            warn!(
                "Failed to update GitLab status {} for drv {:?}: {:?}",
                commit_status.status_id, drv_id, e
            );
        } else {
            debug!(
                "Successfully updated GitLab status {} for drv {:?}",
                commit_status.status_id, drv_id
            );

            // Update our local database tracking
            let state: crate::gitlab::types::GitLabStatusState = status.clone().into();
            let state_str = format!("{:?}", state).to_lowercase();
            if let Err(e) = crate::db::gitlab::update_commit_status_state(
                commit_status.status_id,
                &state_str,
                db_pool,
            )
            .await
            {
                warn!("Failed to update commit status state in database: {:?}", e);
            }
        }
    }

    Ok(())
}

/// Update build status with a size warning
pub(super) async fn handle_update_build_status_with_size_warning(
    drv_id: &crate::db::model::DrvId,
    _status: &crate::db::model::build_event::DrvBuildState,
    baseline_size: u64,
    current_size: u64,
    increase_percent: f64,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    clients: &HashMap<String, Arc<GitLabClient>>,
) -> Result<()> {
    debug!(
        "Updating GitLab build status with size warning for {:?}",
        drv_id
    );

    // Query all commit statuses associated with this derivation
    let statuses = crate::db::gitlab::commit_statuses_for_drv_path(drv_id, db_pool).await?;

    if statuses.is_empty() {
        debug!("No commit statuses found for drv {:?}", drv_id);
        return Ok(());
    }

    // Update each status with size warning
    for commit_status in statuses {
        let Some(client) = clients.get(&commit_status.domain) else {
            warn!("No GitLab client for domain {}", commit_status.domain);
            continue;
        };

        if let Err(e) = crate::gitlab::actions::update_status_with_size_warning(
            client,
            commit_status.project_id,
            &commit_status.sha,
            &commit_status.name,
            baseline_size,
            current_size,
            increase_percent,
        )
        .await
        {
            warn!(
                "Failed to update GitLab status with size warning for {:?}: {:?}",
                drv_id, e
            );
        } else {
            debug!(
                "Successfully updated GitLab status with size warning for {:?}",
                drv_id
            );
        }
    }

    Ok(())
}

/// Create a failure status for a build
pub(super) async fn handle_create_failure_status(
    drv_id: &crate::db::model::DrvId,
    jobset_id: i64,
    job_attr_name: &str,
    _difference: &crate::github::JobDifference,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    clients: &HashMap<String, Arc<GitLabClient>>,
) -> Result<()> {
    debug!(
        "Creating failure status for drv {:?}, jobset {}, job {}",
        drv_id, jobset_id, job_attr_name
    );

    // Query the jobset to get CI info
    let jobset: Option<(String, i64, String, String, String, i64)> = sqlx::query_as(
        r#"
        SELECT sha, project_id, owner, repo_name, domain, project_id
        FROM GitLabJobSets
        WHERE ROWID = ?
        "#,
    )
    .bind(jobset_id)
    .fetch_optional(db_pool)
    .await?;

    let Some((sha, project_id, owner, repo_name, domain, _)) = jobset else {
        warn!("No jobset found with id {}", jobset_id);
        return Ok(());
    };

    let Some(client) = clients.get(&domain) else {
        warn!("No GitLab client for domain {}", domain);
        return Ok(());
    };

    // Get the drv ROWID for database storage
    let drv_rowid: Option<i64> = sqlx::query_scalar("SELECT ROWID FROM Drv WHERE drv_path = ?")
        .bind(drv_id)
        .fetch_optional(db_pool)
        .await?;

    let Some(drv_rowid) = drv_rowid else {
        warn!("No drv found for path {:?}", drv_id);
        return Ok(());
    };

    let status_name = format!("eka-ci/{}", job_attr_name);
    let description = "Build failed";

    match crate::gitlab::actions::create_failure_status(
        client,
        project_id,
        &sha,
        &status_name,
        description,
    )
    .await
    {
        Ok(status_id) => {
            debug!("Created failure status {} for drv {:?}", status_id, drv_id);

            // Store in database
            if let Err(e) = crate::db::gitlab::insert_commit_status_info(
                status_id,
                &sha,
                &status_name,
                project_id,
                &domain,
                &owner,
                &repo_name,
                drv_rowid,
                "failed",
                db_pool,
            )
            .await
            {
                warn!("Failed to insert commit status into database: {:?}", e);
            }
        },
        Err(e) => {
            warn!(
                "Failed to create failure status for drv {:?}: {:?}",
                drv_id, e
            );
        },
    }

    Ok(())
}

/// Cancel all statuses for a commit
pub(super) async fn handle_cancel_statuses_for_commit(
    ci_info: &GitLabCIInfo,
    client: &GitLabClient,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
) -> Result<()> {
    debug!(
        "Canceling all statuses for commit {} in project {}",
        ci_info.commit, ci_info.project_id
    );

    // Query all active statuses for this commit
    let statuses = crate::db::gitlab::commit_statuses_for_commit(&ci_info.commit, db_pool).await?;

    if statuses.is_empty() {
        debug!("No active statuses found for commit {}", ci_info.commit);
        return Ok(());
    }

    // Cancel each status
    for status in statuses {
        if let Err(e) = crate::gitlab::actions::cancel_commit_status(
            client,
            status.project_id,
            &status.sha,
            &status.name,
        )
        .await
        {
            warn!(
                "Failed to cancel status {} for commit {}: {:?}",
                status.status_id, ci_info.commit, e
            );
        } else {
            debug!(
                "Successfully canceled status {} for commit {}",
                status.status_id, ci_info.commit
            );

            // Update database
            if let Err(e) =
                crate::db::gitlab::update_commit_status_state(status.status_id, "canceled", db_pool)
                    .await
            {
                warn!("Failed to update status state in database: {:?}", e);
            }
        }
    }

    Ok(())
}

/// Create a CI eval job status
pub(super) async fn handle_create_ci_eval_job(
    ci_info: &GitLabCIInfo,
    job_title: &str,
    client: &GitLabClient,
    eval_statuses: &Mutex<HashMap<(String, String), i64>>,
) -> Result<()> {
    debug!(
        "Creating CI eval job status for job '{}' on commit {}",
        job_title, ci_info.commit
    );

    match crate::gitlab::actions::create_ci_eval_job(client, ci_info, job_title).await {
        Ok(status_id) => {
            debug!(
                "Created eval job status {} for job '{}' on commit {}",
                status_id, job_title, ci_info.commit
            );

            // Store in eval_statuses HashMap for later updates
            eval_statuses
                .lock()
                .await
                .insert((ci_info.commit.clone(), job_title.to_string()), status_id);

            info!(
                "Created CI eval job status for job '{}' on commit {}",
                job_title, ci_info.commit
            );
        },
        Err(e) => {
            warn!(
                "Failed to create CI eval job status for job '{}' on commit {}: {:?}",
                job_title, ci_info.commit, e
            );
        },
    }

    Ok(())
}

/// Complete a CI eval job status
pub(super) async fn handle_complete_ci_eval_job(
    ci_info: &GitLabCIInfo,
    job_name: &str,
    success: bool,
    client: &GitLabClient,
    eval_statuses: &Mutex<HashMap<(String, String), i64>>,
) -> Result<()> {
    debug!(
        "Completing CI eval job status for job '{}' with success={} on commit {}",
        job_name, success, ci_info.commit
    );

    // Remove from tracking map
    eval_statuses
        .lock()
        .await
        .remove(&(ci_info.commit.clone(), job_name.to_string()));

    match crate::gitlab::actions::update_ci_eval_job(client, ci_info, job_name, success).await {
        Ok(()) => {
            info!(
                "Completed CI eval job status for job '{}' on commit {} (success={})",
                job_name, ci_info.commit, success
            );
        },
        Err(e) => {
            warn!(
                "Failed to complete CI eval job status for job '{}' on commit {}: {:?}",
                job_name, ci_info.commit, e
            );
        },
    }

    Ok(())
}

/// Create a failed CI eval job status with error details
pub(super) async fn handle_fail_ci_eval_job(
    ci_info: &GitLabCIInfo,
    job_name: &str,
    errors: &[crate::nix::nix_eval_jobs::NixEvalError],
    client: &GitLabClient,
) -> Result<()> {
    debug!(
        "Creating failed CI eval job status for job '{}' on commit {} with {} errors",
        job_name,
        ci_info.commit,
        errors.len()
    );

    match crate::gitlab::actions::fail_ci_eval_job(client, ci_info, job_name, errors).await {
        Ok(status_id) => {
            info!(
                "Created failed CI eval job status {} for job '{}' on commit {} ({} errors)",
                status_id,
                job_name,
                ci_info.commit,
                errors.len()
            );
        },
        Err(e) => {
            warn!(
                "Failed to create failed CI eval job status for job '{}' on commit {}: {:?}",
                job_name, ci_info.commit, e
            );
        },
    }

    Ok(())
}
