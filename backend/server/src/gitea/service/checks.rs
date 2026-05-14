// CI check runs and build status updates for Gitea

use anyhow::Result;
use tracing::{debug, info, warn};

use super::helpers::build_state_to_gitea_state;
use crate::gitea::GiteaClient;
use crate::gitea::types::GiteaCIInfo;

/// Create the initial CI configure gate check for a commit
/// Uses check runs for newer Gitea, commit status for older
pub(super) async fn handle_create_ci_configure_gate(
    ci_info: &GiteaCIInfo,
    client: &GiteaClient,
) -> Result<i64> {
    debug!(
        "Creating CI configure gate for commit {} in {}/{}",
        ci_info.commit, ci_info.owner, ci_info.repo_name
    );

    if client.supports_check_runs() {
        // Use Check Runs API via actions module
        match crate::gitea::actions::create_ci_configure_gate(client, ci_info).await {
            Ok(check_run_id) => {
                info!(
                    "Created CI configure gate check run {} for commit {}",
                    check_run_id, ci_info.commit
                );
                Ok(check_run_id)
            },
            Err(e) => {
                warn!(
                    "Failed to create CI configure gate for commit {}: {:?}",
                    ci_info.commit, e
                );
                Err(e)
            },
        }
    } else {
        // Fallback to Commit Status API for older Gitea
        let request = crate::gitea::client::CreateCommitStatusRequest {
            state: crate::gitea::client::CommitStatusState::Pending,
            target_url: None,
            description: Some("Reading repository configuration...".to_string()),
            context: "EkaCI: Configure".to_string(),
        };

        match client
            .create_commit_status(&ci_info.owner, &ci_info.repo_name, &ci_info.commit, request)
            .await
        {
            Ok(_) => {
                info!(
                    "Created configure gate commit status for commit {}",
                    ci_info.commit
                );
                Ok(0) // No check run ID for commit status
            },
            Err(e) => {
                warn!(
                    "Failed to create configure gate commit status for {}: {:?}",
                    ci_info.commit, e
                );
                Err(e)
            },
        }
    }
}

/// Complete the CI configure gate check
pub(super) async fn handle_complete_ci_configure_gate(
    ci_info: &GiteaCIInfo,
    check_run_id: Option<i64>,
    client: &GiteaClient,
) -> Result<()> {
    debug!(
        "Completing CI configure gate for commit {} in {}/{}",
        ci_info.commit, ci_info.owner, ci_info.repo_name
    );

    if client.supports_check_runs() {
        // Update check run to success via actions module
        if let Some(id) = check_run_id {
            match crate::gitea::actions::update_ci_configure_gate(client, ci_info, id).await {
                Ok(()) => {
                    info!(
                        "Successfully completed CI configure gate check run {} for commit {}",
                        id, ci_info.commit
                    );
                },
                Err(e) => {
                    warn!(
                        "Failed to complete CI configure gate for commit {}: {:?}",
                        ci_info.commit, e
                    );
                },
            }
        }
    } else {
        // Update commit status to success for older Gitea
        let request = crate::gitea::client::CreateCommitStatusRequest {
            state: crate::gitea::client::CommitStatusState::Success,
            target_url: None,
            description: Some("CI configuration validated successfully".to_string()),
            context: "EkaCI: Configure".to_string(),
        };

        match client
            .create_commit_status(&ci_info.owner, &ci_info.repo_name, &ci_info.commit, request)
            .await
        {
            Ok(_) => {
                info!(
                    "Completed configure gate commit status for commit {}",
                    ci_info.commit
                );
            },
            Err(e) => {
                warn!(
                    "Failed to complete configure gate commit status for {}: {:?}",
                    ci_info.commit, e
                );
            },
        }
    }

    Ok(())
}

/// Update the status of a build in Gitea
pub(super) async fn handle_update_build_status(
    drv_id: &crate::db::model::DrvId,
    status: &crate::db::model::build_event::DrvBuildState,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    gitea_clients: &tokio::sync::Mutex<
        std::collections::HashMap<String, std::sync::Arc<GiteaClient>>,
    >,
) -> Result<()> {
    debug!("Updating Gitea build status for {:?}: {:?}", drv_id, status);

    // Find all check runs associated with this derivation
    let check_runs = crate::db::gitea::check_runs_for_drv_path(drv_id, db_pool).await?;

    for check_run in check_runs {
        let Some(client) = gitea_clients.lock().await.get(&check_run.domain).cloned() else {
            warn!("No Gitea client for domain {}", check_run.domain);
            continue;
        };

        // Update the check run via actions module
        match crate::gitea::actions::update_build_check_run(
            &client,
            &check_run.repo_owner,
            &check_run.repo_name,
            check_run.check_run_id,
            status,
        )
        .await
        {
            Ok(()) => {
                // Update database tracking
                let state = build_state_to_gitea_state(status);
                if let Err(e) = crate::db::gitea::update_check_run_status(
                    check_run.check_run_id,
                    &state,
                    db_pool,
                )
                .await
                {
                    warn!("Failed to update check run status in database: {:?}", e);
                }
            },
            Err(e) => {
                warn!(
                    "Failed to update check run {} for {:?}: {:?}",
                    check_run.check_run_id, drv_id, e
                );
            },
        }
    }

    Ok(())
}

/// Handle CreateCIEvalJob task
pub(super) async fn handle_create_ci_eval_job(
    ci_info: &crate::gitea::types::GiteaCIInfo,
    job_title: &str,
    client: &GiteaClient,
) -> Result<i64> {
    debug!(
        "Creating CI eval job for job '{}' on commit {} in {}/{}",
        job_title, ci_info.commit, ci_info.owner, ci_info.repo_name
    );

    match crate::gitea::actions::create_ci_eval_job(client, ci_info, job_title).await {
        Ok(check_run_id) => {
            info!(
                "Created CI eval job check run {} for job '{}' on commit {}",
                check_run_id, job_title, ci_info.commit
            );
            Ok(check_run_id)
        },
        Err(e) => {
            warn!(
                "Failed to create CI eval job for job '{}': {:?}",
                job_title, e
            );
            Err(e)
        },
    }
}

/// Handle CompleteCIEvalJob task
pub(super) async fn handle_complete_ci_eval_job(
    ci_info: &crate::gitea::types::GiteaCIInfo,
    job_name: &str,
    check_run_id: i64,
    conclusion: &crate::gitea::types::GiteaCheckConclusion,
    client: &GiteaClient,
) -> Result<()> {
    debug!(
        "Completing CI eval job for job '{}' on commit {} with conclusion {:?}",
        job_name, ci_info.commit, conclusion
    );

    let success = matches!(
        conclusion,
        crate::gitea::types::GiteaCheckConclusion::Success
    );
    match crate::gitea::actions::update_ci_eval_job(
        client,
        ci_info,
        job_name,
        check_run_id,
        success,
    )
    .await
    {
        Ok(()) => {
            info!(
                "Successfully completed CI eval job check run {} for job '{}'",
                check_run_id, job_name
            );
        },
        Err(e) => {
            warn!(
                "Failed to complete CI eval job for job '{}': {:?}",
                job_name, e
            );
        },
    }

    Ok(())
}

/// Handle FailCIEvalJob task
pub(super) async fn handle_fail_ci_eval_job(
    ci_info: &crate::gitea::types::GiteaCIInfo,
    job_name: &str,
    errors: &[crate::nix::NixEvalError],
    client: &GiteaClient,
) -> Result<()> {
    debug!(
        "Creating failed CI eval job for job '{}' on commit {} with {} errors",
        job_name,
        ci_info.commit,
        errors.len()
    );

    match crate::gitea::actions::fail_ci_eval_job(client, ci_info, job_name, errors).await {
        Ok(check_run_id) => {
            info!(
                "Created failed CI eval job check run {} for job '{}' on commit {} ({} errors)",
                check_run_id,
                job_name,
                ci_info.commit,
                errors.len()
            );
        },
        Err(e) => {
            warn!(
                "Failed to create failed CI eval job for job '{}': {:?}",
                job_name, e
            );
        },
    }

    Ok(())
}

/// Handle CancelCheckRunsForCommit task
pub(super) async fn handle_cancel_check_runs_for_commit(
    ci_info: &crate::gitea::types::GiteaCIInfo,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    gitea_clients: &tokio::sync::Mutex<
        std::collections::HashMap<String, std::sync::Arc<GiteaClient>>,
    >,
) -> Result<()> {
    debug!(
        "Canceling check runs for commit {} in {}/{}",
        ci_info.commit, ci_info.owner, ci_info.repo_name
    );

    // Find all active check runs for this commit
    let check_runs = crate::db::gitea::check_runs_for_commit(&ci_info.commit, db_pool).await?;

    for check_run in check_runs {
        let Some(client) = gitea_clients.lock().await.get(&check_run.domain).cloned() else {
            warn!("No Gitea client for domain {}", check_run.domain);
            continue;
        };

        match crate::gitea::actions::cancel_check_run(
            &client,
            &check_run.repo_owner,
            &check_run.repo_name,
            check_run.check_run_id,
        )
        .await
        {
            Ok(()) => {
                // Update database tracking
                if let Err(e) = crate::db::gitea::update_check_run_status(
                    check_run.check_run_id,
                    "cancelled",
                    db_pool,
                )
                .await
                {
                    warn!("Failed to update check run status in database: {:?}", e);
                }
            },
            Err(e) => {
                warn!(
                    "Failed to cancel check run {}: {:?}",
                    check_run.check_run_id, e
                );
            },
        }
    }

    Ok(())
}

/// Handle UpdateBuildStatusWithSizeWarning task
pub(super) async fn handle_update_build_status_with_size_warning(
    drv_id: &crate::db::model::DrvId,
    status: &crate::db::model::build_event::DrvBuildState,
    baseline_size: u64,
    current_size: u64,
    increase_percent: f64,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    gitea_clients: &tokio::sync::Mutex<
        std::collections::HashMap<String, std::sync::Arc<GiteaClient>>,
    >,
) -> Result<()> {
    debug!(
        "Updating Gitea build status with size warning for {:?}: {:?}",
        drv_id, status
    );

    // Find all check runs associated with this derivation
    let check_runs = crate::db::gitea::check_runs_for_drv_path(drv_id, db_pool).await?;

    let warning_message = format!(
        "Build succeeded with size warning: {}% increase ({} → {})",
        increase_percent as u64,
        crate::nix::size::format_size(baseline_size),
        crate::nix::size::format_size(current_size)
    );

    for check_run in check_runs {
        let Some(client) = gitea_clients.lock().await.get(&check_run.domain).cloned() else {
            warn!("No Gitea client for domain {}", check_run.domain);
            continue;
        };

        // Create an update request with the size warning in the output
        let request = crate::gitea::client::UpdateCheckRunRequest {
            status: Some(crate::gitea::client::CheckStatus::Completed),
            conclusion: Some(crate::gitea::client::CheckConclusion::Success),
            output: Some(crate::gitea::client::CheckOutput {
                title: "Build Succeeded with Size Warning".to_string(),
                summary: warning_message.clone(),
                text: None,
            }),
        };

        match client
            .update_check_run(
                &check_run.repo_owner,
                &check_run.repo_name,
                check_run.check_run_id,
                request,
            )
            .await
        {
            Ok(()) => {
                // Update database tracking
                if let Err(e) = crate::db::gitea::update_check_run_status(
                    check_run.check_run_id,
                    "success",
                    db_pool,
                )
                .await
                {
                    warn!("Failed to update check run status in database: {:?}", e);
                }
            },
            Err(e) => {
                warn!(
                    "Failed to update check run {} with size warning: {:?}",
                    check_run.check_run_id, e
                );
            },
        }
    }

    Ok(())
}

/// Handle CreateFailureCheckRun task
pub(super) async fn handle_create_failure_check_run(
    drv_id: &crate::db::model::DrvId,
    jobset_id: i64,
    job_attr_name: &str,
    difference: &crate::github::JobDifference,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    gitea_clients: &tokio::sync::Mutex<
        std::collections::HashMap<String, std::sync::Arc<GiteaClient>>,
    >,
) -> Result<()> {
    debug!(
        "Creating failure check run for {:?} (jobset {}, job '{}')",
        drv_id, jobset_id, job_attr_name
    );

    // Get jobset information to find the commit and repository
    let jobset_info: Option<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT sha, job, owner, repo_name, domain FROM GiteaJobSets WHERE ROWID = ?",
    )
    .bind(jobset_id)
    .fetch_optional(db_pool)
    .await?;

    let Some((sha, _job, owner, repo_name, domain)) = jobset_info else {
        warn!("No jobset found with ROWID {}", jobset_id);
        return Ok(());
    };

    let Some(client) = gitea_clients.lock().await.get(&domain).cloned() else {
        warn!("No Gitea client for domain {}", domain);
        return Ok(());
    };

    // Build description based on the difference type
    let description = match difference {
        crate::github::JobDifference::New => {
            format!("Job '{}' is new in this build", job_attr_name)
        },
        crate::github::JobDifference::Changed => {
            format!("Job '{}' failed: derivation changed", job_attr_name)
        },
        crate::github::JobDifference::Removed => {
            format!("Job '{}' was removed from the build set", job_attr_name)
        },
    };

    let check_name = format!("eka-ci/build/{}", job_attr_name);

    match crate::gitea::actions::create_failure_check_run(
        &client,
        &owner,
        &repo_name,
        &sha,
        &check_name,
        &description,
    )
    .await
    {
        Ok(check_run_id) => {
            info!(
                "Created failure check run {} for job '{}' on commit {}",
                check_run_id, job_attr_name, sha
            );

            // Get drv ROWID for database insertion
            if let Ok(Some(drv_rowid)) =
                sqlx::query_scalar::<_, i64>("SELECT ROWID FROM Drv WHERE drv_path = ?")
                    .bind(drv_id)
                    .fetch_optional(db_pool)
                    .await
            {
                // Store in database for tracking
                if let Err(e) = crate::db::gitea::insert_check_run_info(
                    check_run_id,
                    &sha,
                    &check_name,
                    &domain,
                    &owner,
                    &repo_name,
                    drv_rowid,
                    "failure",
                    db_pool,
                )
                .await
                {
                    warn!("Failed to insert check run info in database: {:?}", e);
                }
            }
        },
        Err(e) => {
            warn!(
                "Failed to create failure check run for job '{}': {:?}",
                job_attr_name, e
            );
        },
    }

    Ok(())
}
