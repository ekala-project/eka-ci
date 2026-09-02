// Pull request webhook handling

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, bail};
use octocrab::models::pulls::PullRequest;
use octocrab::models::webhook_events::payload;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::config::GitHubAppConfig;
use crate::db::DbService;
use crate::git::GitTask;
use crate::github::{CICheckInfo, GitHubTask};
use crate::github_permissions::{PermissionContext, check_github_app_permission};

pub(super) async fn is_valid_user(pr: &PullRequest, db_service: &DbService) -> anyhow::Result<()> {
    let pr_author = &pr.user.as_ref().context("Missing github user for PR")?;
    let username = &pr_author.login;
    let user_id = pr_author.id.0 as i64;

    if !db_service.is_user_approved(username, user_id).await? {
        bail!("User is not approved for workflows");
    }

    Ok(())
}

/// Store PR metadata in the database
pub(super) async fn store_pr_metadata(pr: &PullRequest, state: &str, db_service: &DbService) {
    let pr_author = match &pr.user {
        Some(user) => &user.login,
        None => {
            warn!(
                "PR #{} has no user information, skipping metadata storage",
                pr.number
            );
            return;
        },
    };

    let pr_number = pr.number as i64;

    let owner = match &pr.base.repo {
        Some(repo) => match &repo.owner {
            Some(owner) => &owner.login,
            None => {
                warn!("PR base repo has no owner");
                return;
            },
        },
        None => {
            warn!("PR base has no repo");
            return;
        },
    };

    let repo_name = match &pr.base.repo {
        Some(repo) => &repo.name,
        None => {
            warn!("PR base has no repo");
            return;
        },
    };

    let head_sha = &pr.head.sha;
    let base_sha = &pr.base.sha;

    let title = match &pr.title {
        Some(t) => t.as_str(),
        None => "",
    };

    let created_at = pr.created_at.map(|dt| dt.to_rfc3339()).unwrap_or_default();
    let updated_at = pr.updated_at.map(|dt| dt.to_rfc3339()).unwrap_or_default();

    if let Err(e) = db_service
        .upsert_pull_request(
            pr_number,
            owner,
            repo_name,
            head_sha,
            base_sha,
            title,
            pr_author,
            state,
            &created_at,
            &updated_at,
        )
        .await
    {
        warn!("Failed to store PR metadata for PR #{}: {:?}", pr_number, e);
    } else {
        debug!("Stored metadata for PR #{}", pr_number);
    }
}

/// On `Synchronize`, cancel any pending `@eka-ci merge` whose pinned SHA
/// no longer matches the head. Posts a notification, reacts `:confused:`
/// on the original command, and clears the DB fields. Per-step failures
/// are logged but non-fatal.
pub(super) async fn check_comment_merge_drift(
    pr: &PullRequest,
    db_service: &DbService,
    github_sender: &mpsc::Sender<GitHubTask>,
) {
    let Some(base_repo) = pr.base.repo.as_ref() else {
        return;
    };
    let Some(owner) = base_repo.owner.as_ref().map(|o| o.login.clone()) else {
        return;
    };
    let repo_name = base_repo.name.clone();
    let pr_number = pr.number as i64;

    let pr_row = match crate::db::github::get_pull_request_row(
        &owner,
        &repo_name,
        pr_number,
        &db_service.pool,
    )
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return,
        Err(e) => {
            warn!(
                "Failed to look up PR row for drift check on {}/{}#{}: {:?}",
                owner, repo_name, pr_number, e
            );
            return;
        },
    };

    let Some(pending) = pr_row.pending_comment_merge() else {
        return; // nothing pending
    };

    if pending.sha == pr.head.sha {
        // Possible on a no-op force-push; just skip.
        return;
    }

    debug!(
        "Comment-merge SHA drift on {}/{}#{}: pinned={} new_head={}",
        owner, repo_name, pr_number, pending.sha, pr.head.sha
    );

    if let Err(e) = github_sender
        .send(GitHubTask::CommentMergeDriftCancelled {
            owner: owner.clone(),
            repo_name: repo_name.clone(),
            pr_number,
            expected_sha: pending.sha.clone(),
            actual_sha: pr.head.sha.clone(),
            requester_login: pending.requester_login.clone(),
        })
        .await
    {
        warn!(
            "Failed to enqueue CommentMergeDriftCancelled for {}/{}#{}: {:?}",
            owner, repo_name, pr_number, e
        );
    }

    // `:confused:` the original command so the thread reflects the cancel.
    if let Err(e) = github_sender
        .send(GitHubTask::ReactToComment {
            owner: owner.clone(),
            repo_name: repo_name.clone(),
            comment_id: pending.comment_id,
            content: "confused".to_string(),
        })
        .await
    {
        warn!(
            "Failed to enqueue drift-cancel reaction on {}/{}#{}: {:?}",
            owner, repo_name, pr_number, e
        );
    }

    // Clear pending so downstream schedulers don't re-fire on the drifted head.
    if let Err(e) = crate::db::github::clear_comment_merge_request(
        &owner,
        &repo_name,
        pr_number,
        &db_service.pool,
    )
    .await
    {
        warn!(
            "Failed to clear comment_merge_request for {}/{}#{}: {:?}",
            owner, repo_name, pr_number, e
        );
    }
}

pub(super) async fn handle_github_pr(
    pr: payload::PullRequestWebhookEventPayload,
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    require_approval: bool,
    db_service: DbService,
    github_app_configs: Arc<HashMap<String, GitHubAppConfig>>,
) {
    use payload::PullRequestWebhookEventAction as PRWEA;

    // This handler should only exist if a valid github_sender also exsits,
    // should be safe to assume this will always succeed
    let github_sender = match github_sender {
        Some(sender) => sender,
        _ => {
            warn!("GitHub service is down, unable to service webhook request. Restart Eka-CI");
            return;
        },
    };

    match pr.action {
        PRWEA::Opened | PRWEA::Synchronize | PRWEA::Reopened => {
            debug!("Received event for PR #{}", &pr.pull_request.number);

            // Check GitHub App permissions before processing
            if let Some(base_repo) = &pr.pull_request.base.repo {
                let owner = &base_repo
                    .owner
                    .as_ref()
                    .map(|o| o.login.clone())
                    .unwrap_or_default();
                let repo_name = &base_repo.name;
                let branch = &pr.pull_request.base.ref_field;

                let permission_context = PermissionContext {
                    repo_owner: owner.clone(),
                    repo_name: repo_name.clone(),
                    branch: Some(branch.clone()),
                };

                // Check if any configured GitHub App has permission for this repo/branch
                let has_permission = github_app_configs
                    .values()
                    .any(|config| check_github_app_permission(config, &permission_context).is_ok());

                if !has_permission {
                    warn!(
                        "No GitHub App has permission for repository {}/{} branch {}. Skipping PR \
                         #{}",
                        owner, repo_name, branch, pr.pull_request.number
                    );
                    return;
                }
            }

            // Store PR metadata in database
            store_pr_metadata(&pr.pull_request, "open", &db_service).await;

            // SHA-drift cancellation for pending @eka-ci merge commands.
            // `@eka-ci merge` is SHA-pinned: once a user issues the command,
            // we refuse to merge a newer head (new commits imply unreviewed
            // changes). On Synchronize, if the stored pinned sha differs
            // from the new head, clear the pending request and notify the
            // requester. Opened/Reopened can't have drifted state worth
            // cancelling (no prior comment-merge on a just-opened PR; a
            // reopened PR's previous pending merge would already have been
            // cleared at close-time).
            if matches!(pr.action, PRWEA::Synchronize) {
                check_comment_merge_drift(&pr.pull_request, &db_service, &github_sender).await;
            }

            if require_approval && is_valid_user(&pr.pull_request, &db_service).await.is_err() {
                let username = match &pr.pull_request.user {
                    Some(user) => user.login.clone(),
                    None => {
                        warn!(
                            "PR #{} has no user information, skipping approval gate",
                            pr.pull_request.number
                        );
                        return;
                    },
                };
                let ci_check_info = match CICheckInfo::from_gh_pr_head(&pr.pull_request) {
                    Ok(info) => info,
                    Err(e) => {
                        warn!(
                            "PR #{} missing head repo/owner data, cannot create approval check: \
                             {:?}",
                            pr.pull_request.number, e
                        );
                        return;
                    },
                };
                if let Err(e) = github_sender
                    .send(GitHubTask::CreateApprovalRequiredCheckRun {
                        ci_check_info: std::sync::Arc::new(ci_check_info),
                        username,
                    })
                    .await
                {
                    warn!("Failed to send approval required check run task: {:?}", e);
                }

                // Don't proceed until workflow is approved
                return;
            }

            let git_task = GitTask::GitHubCheckout(pr.pull_request.clone());

            if let Err(e) = git_sender.send(git_task).await {
                warn!("Failed to send PR checkout task: {:?}", e);
            } else {
                debug!(
                    "Successfully queued PR checkout task for PR #{}",
                    pr.pull_request.number
                );
            }
        },
        PRWEA::Closed | PRWEA::ConvertedToDraft => {
            debug!(
                "Received close/draft event for PR #{}, cancelling check runs",
                pr.pull_request.number
            );

            // Update PR state in database
            let state = if matches!(pr.action, PRWEA::Closed) {
                // Determine if merged or just closed
                if pr.pull_request.merged_at.is_some() {
                    "merged"
                } else {
                    "closed"
                }
            } else {
                "open" // ConvertedToDraft keeps it open
            };
            store_pr_metadata(&pr.pull_request, state, &db_service).await;

            let ci_check_info = match CICheckInfo::from_gh_pr_head(&pr.pull_request) {
                Ok(info) => info,
                Err(e) => {
                    warn!(
                        "PR #{} missing head repo/owner data, cannot cancel check runs: {:?}",
                        pr.pull_request.number, e
                    );
                    return;
                },
            };

            if let Err(e) = github_sender
                .send(GitHubTask::CancelCheckRunsForCommit {
                    ci_check_info: std::sync::Arc::new(ci_check_info),
                })
                .await
            {
                warn!(
                    "Failed to send cancellation task for PR #{}: {:?}",
                    pr.pull_request.number, e
                );
            } else {
                debug!(
                    "Successfully queued cancellation task for PR #{}",
                    pr.pull_request.number
                );
            }
        },
        PRWEA::Enqueued => {
            debug!(
                "Received enqueued event for PR #{}",
                &pr.pull_request.number
            );

            // Store PR metadata in database
            store_pr_metadata(&pr.pull_request, "open", &db_service).await;

            if require_approval && is_valid_user(&pr.pull_request, &db_service).await.is_err() {
                let username = match &pr.pull_request.user {
                    Some(user) => user.login.clone(),
                    None => {
                        warn!(
                            "PR #{} has no user information, skipping approval gate",
                            pr.pull_request.number
                        );
                        return;
                    },
                };
                let ci_check_info = match CICheckInfo::from_gh_pr_head(&pr.pull_request) {
                    Ok(info) => info,
                    Err(e) => {
                        warn!(
                            "PR #{} missing head repo/owner data, cannot create approval check: \
                             {:?}",
                            pr.pull_request.number, e
                        );
                        return;
                    },
                };
                if let Err(e) = github_sender
                    .send(GitHubTask::CreateApprovalRequiredCheckRun {
                        ci_check_info: std::sync::Arc::new(ci_check_info),
                        username,
                    })
                    .await
                {
                    warn!("Failed to send approval required check run task: {:?}", e);
                }

                // Don't proceed until workflow is approved
                return;
            }

            let git_task = GitTask::GitHubCheckout(pr.pull_request.clone());

            if let Err(e) = git_sender.send(git_task).await {
                warn!("Failed to send PR checkout task for enqueued PR: {:?}", e);
            } else {
                debug!(
                    "Successfully queued PR checkout task for enqueued PR #{}",
                    pr.pull_request.number
                );
            }
        },
        action => {
            debug!("Ignoring non-actionable PR action: {:?}", &action);
        },
    }
}
