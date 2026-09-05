// Comment command processing and authorization for Gitea PRs

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::db::DbService;
use crate::gitea::GiteaClient;
use crate::gitea::service::helpers::{Authorization, short_sha};
use crate::gitea::types::GiteaTask;

/// Process a merge command from a PR comment
#[allow(clippy::too_many_arguments)]
pub(in crate::gitea::service) async fn handle_process_merge_command(
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    comment_id: i64,
    requester_id: i64,
    requester_login: &str,
    body: &str,
    comment_created_at: &chrono::DateTime<chrono::Utc>,
    client: &GiteaClient,
    db_service: &DbService,
    gitea_sender: &tokio::sync::mpsc::Sender<GiteaTask>,
) -> Result<()> {
    use crate::gitea::webhook::comment_command::{CommentCommand, parse_comment_command};

    // Re-parse rather than carrying a typed command
    let Some(cmd) = parse_comment_command(body) else {
        debug!(
            "Comment {} on PR #{} in {}/{} no longer parses as a command; dropping",
            comment_id, pr_number, owner, repo_name
        );
        return Ok(());
    };

    match cmd {
        CommentCommand::MergeCancel => {
            handle_merge_cancel(
                client,
                domain,
                owner,
                repo_name,
                pr_number,
                requester_id,
                requester_login,
                db_service,
            )
            .await
        },
        CommentCommand::Merge { method } => {
            handle_merge_accept(
                client,
                domain,
                owner,
                repo_name,
                pr_number,
                comment_id,
                requester_id,
                requester_login,
                method.as_deref(),
                comment_created_at,
                db_service,
                gitea_sender,
            )
            .await
        },
    }
}

/// Outcome of an authorization check against a commenter.
async fn authorize_commenter(
    client: &GiteaClient,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    requester_id: i64,
    requester_login: &str,
    db_service: &DbService,
) -> Result<Authorization> {
    let perm = match crate::gitea::actions::check_repo_permission_for_user(
        client,
        owner,
        repo_name,
        requester_login,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "Failed to check repo permission for {} on {}/{}: {:?}",
                requester_login, owner, repo_name, e
            );
            return Ok(Authorization::Abort);
        },
    };
    let has_write = perm.can_push || perm.is_admin;

    if has_write {
        return Ok(Authorization::Granted { has_write: true });
    }

    let changed = crate::db::gitea::get_pr_changed_packages(
        client.get_domain(),
        owner,
        repo_name,
        pr_number,
        &db_service.pool,
    )
    .await
    .unwrap_or_default();

    let is_pkg_maintainer = if changed.is_empty() {
        false
    } else {
        crate::db::maintainers::is_maintainer_of_all_packages(
            requester_id,
            &changed,
            &db_service.pool,
        )
        .await
        .unwrap_or(false)
    };

    if is_pkg_maintainer {
        Ok(Authorization::Granted { has_write: false })
    } else {
        Ok(Authorization::Denied)
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_merge_cancel(
    client: &GiteaClient,
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    requester_id: i64,
    requester_login: &str,
    db_service: &DbService,
) -> Result<()> {
    // Silent no-op when nothing is pending
    let pr_row = crate::db::gitea::get_pull_request_row(
        domain,
        owner,
        repo_name,
        pr_number,
        &db_service.pool,
    )
    .await?;

    let Some(pending) = pr_row.as_ref().and_then(|p| p.pending_comment_merge()) else {
        debug!(
            "No pending comment-merge on PR #{} in {}/{}; ignoring cancel from {}",
            pr_number, owner, repo_name, requester_login
        );
        return Ok(());
    };

    let is_self = requester_id == pending.requester_id;

    // Self-cancel fast path: skip API permission lookup
    let authorized = if is_self {
        true
    } else {
        match authorize_commenter(
            client,
            owner,
            repo_name,
            pr_number,
            requester_id,
            requester_login,
            db_service,
        )
        .await?
        {
            Authorization::Granted { .. } => true,
            Authorization::Denied => false,
            Authorization::Abort => return Ok(()),
        }
    };

    if !authorized {
        info!(
            "Denying @eka-ci merge cancel from {} on PR #{}: not the original requester, no repo \
             write, and not a maintainer of all changed packages",
            requester_login, pr_number
        );
        if let Err(e) = client
            .create_issue_comment(
                owner,
                repo_name,
                pr_number,
                &format!(
                    "@{} I can't cancel this merge request — you must be the original requester, \
                     have write access to the repository, or be a maintainer of all affected \
                     packages.",
                    requester_login
                ),
            )
            .await
        {
            warn!("Failed to post denial comment: {:?}", e);
        }
        return Ok(());
    }

    // Authorized: clear the pending request
    crate::db::gitea::clear_comment_merge(domain, owner, repo_name, pr_number, &db_service.pool)
        .await?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn handle_merge_accept(
    client: &GiteaClient,
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    comment_id: i64,
    requester_id: i64,
    requester_login: &str,
    method_str: Option<&str>,
    comment_created_at: &chrono::DateTime<chrono::Utc>,
    db_service: &DbService,
    gitea_sender: &tokio::sync::mpsc::Sender<GiteaTask>,
) -> Result<()> {
    match authorize_commenter(
        client,
        owner,
        repo_name,
        pr_number,
        requester_id,
        requester_login,
        db_service,
    )
    .await?
    {
        Authorization::Granted { .. } => {},
        Authorization::Abort => return Ok(()),
        Authorization::Denied => {
            info!(
                "Denying @eka-ci merge from {} on PR #{}: no repo write and not a maintainer of \
                 all changed packages",
                requester_login, pr_number
            );
            if let Err(e) = client
                .create_issue_comment(
                    owner,
                    repo_name,
                    pr_number,
                    &format!(
                        "@{} I can't merge this PR — you need write access to the repository or \
                         be a maintainer of all affected packages.",
                        requester_login
                    ),
                )
                .await
            {
                warn!("Failed to post denial comment: {:?}", e);
            }
            return Ok(());
        },
    }

    // Pin the merge to the current head SHA
    let Some(pr) = crate::db::gitea::get_pull_request_row(
        domain,
        owner,
        repo_name,
        pr_number,
        &db_service.pool,
    )
    .await?
    else {
        warn!(
            "PR #{} in {}/{} not found when processing merge command",
            pr_number, owner, repo_name
        );
        return Ok(());
    };

    if !check_push_timing(
        client,
        owner,
        repo_name,
        pr_number,
        requester_login,
        &pr.head_sha,
        comment_created_at,
    )
    .await?
    {
        return Ok(());
    }

    let rows = crate::db::gitea::set_comment_merge(
        domain,
        owner,
        repo_name,
        pr_number,
        &pr.head_sha,
        method_str,
        requester_id,
        requester_login,
        comment_id,
        &db_service.pool,
    )
    .await?;

    if rows == 0 {
        warn!(
            "set_comment_merge affected 0 rows for PR #{} in {}/{}",
            pr_number, owner, repo_name
        );
        return Ok(());
    }

    // Fire the evaluator in case gates are already green
    if let Err(e) = gitea_sender
        .send(GiteaTask::CheckAutoMerge {
            domain: domain.to_string(),
            owner: owner.to_string(),
            repo_name: repo_name.to_string(),
            pr_number,
        })
        .await
    {
        warn!("Failed to send CheckAutoMerge task: {:?}", e);
    }

    Ok(())
}

/// Best-effort force-push detection
#[allow(clippy::too_many_arguments)]
async fn check_push_timing(
    client: &GiteaClient,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    requester_login: &str,
    head_sha: &str,
    comment_created_at: &chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    const PUSH_GRACE: chrono::Duration = chrono::Duration::seconds(30);

    match crate::gitea::actions::fetch_head_commit_date(client, owner, repo_name, head_sha).await {
        Ok(Some(commit_date)) if commit_date > *comment_created_at + PUSH_GRACE => {
            info!(
                "Refusing @eka-ci merge from {} on PR #{}: head commit {} committed at {} is \
                 newer than the command comment at {} (grace={}s); likely post-command push",
                requester_login,
                pr_number,
                head_sha,
                commit_date,
                comment_created_at,
                PUSH_GRACE.num_seconds()
            );
            if let Err(e) = client
                .create_issue_comment(
                    owner,
                    repo_name,
                    pr_number,
                    &format!(
                        "@{} I can't merge this PR — the head commit (`{}`) appears to have been \
                         pushed after your `@eka-ci merge` command. Please review the latest \
                         changes and re-issue the command if you still want to merge.",
                        requester_login,
                        short_sha(head_sha),
                    ),
                )
                .await
            {
                warn!("Failed to post push drift comment: {:?}", e);
            }
            Ok(false)
        },
        Ok(Some(_)) | Ok(None) | Err(_) => Ok(true),
    }
}
