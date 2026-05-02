// GitHub App installation webhook handling

use octocrab::models::webhook_events::{EventInstallation, payload};
use tracing::{debug, warn};

use crate::db::DbService;

pub(super) async fn handle_github_installation(
    installation_payload: payload::InstallationWebhookEventPayload,
    installation: Option<EventInstallation>,
    db_service: DbService,
) {
    use payload::InstallationWebhookEventAction as IWEA;

    // Extract the full installation object from the event
    let installation = match installation {
        Some(EventInstallation::Full(inst)) => inst,
        _ => {
            warn!("Installation event missing full installation object");
            return;
        },
    };

    let installation_id = installation.id.0 as i64;
    let account_type = &installation.account.r#type;
    let account_login = &installation.account.login;

    debug!(
        "Received installation event: {:?} for installation {} ({})",
        installation_payload.action, installation_id, account_login
    );

    match installation_payload.action {
        IWEA::Created => {
            // App was installed
            if let Err(e) = db_service
                .upsert_installation(installation_id, account_type, account_login)
                .await
            {
                warn!(
                    "Failed to upsert installation {} for {}: {}",
                    installation_id, account_login, e
                );
                return;
            }

            // Add all repositories that came with the installation
            if let Some(repos) = installation_payload.repositories {
                for repo in repos {
                    let repo_id = repo.id.0 as i64;
                    let repo_name = &repo.name;
                    // Extract owner from full_name (format: "owner/repo")
                    let repo_owner = repo.full_name.split('/').next().unwrap_or(account_login);

                    if let Err(e) = db_service
                        .upsert_installation_repository(
                            installation_id,
                            repo_id,
                            repo_name,
                            repo_owner,
                        )
                        .await
                    {
                        warn!(
                            "Failed to add repository {} to installation {}: {}",
                            repo_name, installation_id, e
                        );
                    } else {
                        debug!(
                            "Added repository {}/{} to installation {}",
                            repo_owner, repo_name, installation_id
                        );
                    }
                }
            }
        },
        IWEA::Deleted => {
            // App was uninstalled
            if let Err(e) = db_service.delete_installation(installation_id).await {
                warn!("Failed to delete installation {}: {}", installation_id, e);
            } else {
                debug!("Deleted installation {}", installation_id);
            }
        },
        IWEA::Suspend => {
            // App was suspended
            if let Err(e) = db_service.suspend_installation(installation_id).await {
                warn!("Failed to suspend installation {}: {}", installation_id, e);
            } else {
                debug!("Suspended installation {}", installation_id);
            }
        },
        IWEA::Unsuspend => {
            // App was unsuspended
            if let Err(e) = db_service.unsuspend_installation(installation_id).await {
                warn!(
                    "Failed to unsuspend installation {}: {}",
                    installation_id, e
                );
            } else {
                debug!("Unsuspended installation {}", installation_id);
            }
        },
        action => {
            debug!(
                "Ignoring installation action {:?} for installation {}",
                action, installation_id
            );
        },
    }
}

pub(super) async fn handle_github_installation_repositories(
    installation_repos_payload: payload::InstallationRepositoriesWebhookEventPayload,
    installation: Option<EventInstallation>,
    db_service: DbService,
) {
    use payload::InstallationRepositoriesWebhookEventAction as IRWEA;

    // Extract the installation object from the event
    let installation = match installation {
        Some(EventInstallation::Full(inst)) => inst,
        Some(EventInstallation::Minimal(inst)) => {
            // For installation_repositories events, we might get minimal installation
            // We can still work with just the ID
            debug!("Got minimal installation: {:?}", inst);
            // We'll need the installation ID
            let installation_id = inst.id.0 as i64;

            debug!(
                "Received installation_repositories event: {:?} for installation {}",
                installation_repos_payload.action, installation_id
            );

            match installation_repos_payload.action {
                IRWEA::Added => {
                    // Repositories were added to the installation
                    for repo in installation_repos_payload.repositories_added {
                        let repo_id = repo.id.0 as i64;
                        let repo_name = &repo.name;
                        // Extract owner from full_name (format: "owner/repo")
                        let repo_owner = repo.full_name.split('/').next().unwrap_or("");

                        if let Err(e) = db_service
                            .upsert_installation_repository(
                                installation_id,
                                repo_id,
                                repo_name,
                                repo_owner,
                            )
                            .await
                        {
                            warn!(
                                "Failed to add repository {} to installation {}: {}",
                                repo_name, installation_id, e
                            );
                        } else {
                            debug!(
                                "Added repository {}/{} to installation {}",
                                repo_owner, repo_name, installation_id
                            );
                        }
                    }
                },
                IRWEA::Removed => {
                    // Repositories were removed from the installation
                    for repo in installation_repos_payload.repositories_removed {
                        let repo_id = repo.id.0 as i64;

                        if let Err(e) = db_service
                            .delete_installation_repository(installation_id, repo_id)
                            .await
                        {
                            warn!(
                                "Failed to remove repository {} from installation {}: {}",
                                repo_id, installation_id, e
                            );
                        } else {
                            debug!(
                                "Removed repository {} from installation {}",
                                repo_id, installation_id
                            );
                        }
                    }
                },
                action => {
                    debug!(
                        "Ignoring installation_repositories action {:?} for installation {}",
                        action, installation_id
                    );
                },
            }
            return;
        },
        None => {
            warn!("Installation_repositories event missing installation object");
            return;
        },
    };

    let installation_id = installation.id.0 as i64;

    debug!(
        "Received installation_repositories event: {:?} for installation {}",
        installation_repos_payload.action, installation_id
    );

    match installation_repos_payload.action {
        IRWEA::Added => {
            // Repositories were added to the installation
            for repo in installation_repos_payload.repositories_added {
                let repo_id = repo.id.0 as i64;
                let repo_name = &repo.name;
                // Extract owner from full_name (format: "owner/repo")
                let repo_owner = repo
                    .full_name
                    .split('/')
                    .next()
                    .unwrap_or(&installation.account.login);

                if let Err(e) = db_service
                    .upsert_installation_repository(installation_id, repo_id, repo_name, repo_owner)
                    .await
                {
                    warn!(
                        "Failed to add repository {} to installation {}: {}",
                        repo_name, installation_id, e
                    );
                } else {
                    debug!(
                        "Added repository {}/{} to installation {}",
                        repo_owner, repo_name, installation_id
                    );
                }
            }
        },
        IRWEA::Removed => {
            // Repositories were removed from the installation
            for repo in installation_repos_payload.repositories_removed {
                let repo_id = repo.id.0 as i64;

                if let Err(e) = db_service
                    .delete_installation_repository(installation_id, repo_id)
                    .await
                {
                    warn!(
                        "Failed to remove repository {} from installation {}: {}",
                        repo_id, installation_id, e
                    );
                } else {
                    debug!(
                        "Removed repository {} from installation {}",
                        repo_id, installation_id
                    );
                }
            }
        },
        action => {
            debug!(
                "Ignoring installation_repositories action {:?} for installation {}",
                action, installation_id
            );
        },
    }
}
