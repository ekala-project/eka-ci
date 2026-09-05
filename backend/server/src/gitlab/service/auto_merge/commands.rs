// GitLab merge command processing and authorization

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::gitlab::GitLabClient;
use crate::gitlab::service::helpers::{Authorization, short_sha};
use crate::gitlab::types::GitLabTask;

/// Process a merge command from an MR note
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_process_merge_command(
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    note_id: i64,
    requester_id: i64,
    requester_username: &str,
    body: &str,
    note_created_at: &chrono::DateTime<chrono::Utc>,
    client: &GitLabClient,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    gitlab_sender: &tokio::sync::mpsc::Sender<GitLabTask>,
) -> Result<()> {
    use crate::gitlab::webhook::comment_command::{CommentCommand, parse_comment_command};

    // Re-parse rather than carrying a typed command
    let Some(cmd) = parse_comment_command(body) else {
        debug!(
            "Note {} on MR !{} in project {} no longer parses as a command; dropping",
            note_id, mr_iid, project_id
        );
        return Ok(());
    };

    match cmd {
        CommentCommand::MergeCancel => {
            handle_merge_cancel(
                client,
                domain,
                project_id,
                mr_iid,
                requester_id,
                requester_username,
                db_pool,
            )
            .await
        },
        CommentCommand::Merge { method } => {
            handle_merge_accept(
                client,
                domain,
                project_id,
                mr_iid,
                note_id,
                requester_id,
                requester_username,
                method.as_deref(),
                note_created_at,
                db_pool,
                gitlab_sender,
            )
            .await
        },
    }
}

/// Outcome of an authorization check against a commenter.
async fn authorize_commenter(
    client: &GitLabClient,
    project_id: i64,
    mr_iid: i64,
    requester_id: i64,
    requester_username: &str,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
) -> Result<Authorization> {
    let perm = match crate::gitlab::actions::check_project_permission_for_user(
        client,
        project_id,
        requester_id,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "Failed to check project permission for {} on project {}: {:?}",
                requester_username, project_id, e
            );
            return Ok(Authorization::Abort);
        },
    };
    let has_write = perm >= 30; // 30 = Developer, 40 = Maintainer, 50 = Owner

    if has_write {
        return Ok(Authorization::Granted { has_write: true });
    }

    let domain = client.get_domain();
    let changed = crate::db::gitlab::get_mr_changed_packages(domain, project_id, mr_iid, db_pool)
        .await
        .unwrap_or_default();

    let is_pkg_maintainer = if changed.is_empty() {
        false
    } else {
        crate::db::maintainers::is_maintainer_of_all_packages(requester_id, &changed, db_pool)
            .await
            .unwrap_or(false)
    };

    if is_pkg_maintainer {
        Ok(Authorization::Granted { has_write: false })
    } else {
        Ok(Authorization::Denied)
    }
}

/// Handle a merge cancel command
async fn handle_merge_cancel(
    client: &GitLabClient,
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    requester_id: i64,
    requester_username: &str,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
) -> Result<()> {
    // Silent no-op when nothing is pending
    let mr_row =
        crate::db::gitlab::get_merge_request_row(domain, project_id, mr_iid, db_pool).await?;

    let Some(pending) = mr_row.as_ref().and_then(|m| m.pending_comment_merge()) else {
        debug!(
            "No pending comment-merge on MR !{} in project {}; ignoring cancel from {}",
            mr_iid, project_id, requester_username
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
            project_id,
            mr_iid,
            requester_id,
            requester_username,
            db_pool,
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
            "Denying @eka-ci merge cancel from {} on MR !{}: not the original requester, no \
             project write, and not a maintainer of all changed packages",
            requester_username, mr_iid
        );
        if let Err(e) = client
            .create_merge_request_note(
                project_id,
                mr_iid,
                &format!(
                    "@{} I can't cancel this merge request — you must be the original requester, \
                     have write access to the project, or be a maintainer of all affected \
                     packages.",
                    requester_username
                ),
            )
            .await
        {
            warn!("Failed to post denial comment: {:?}", e);
        }
        return Ok(());
    }

    // Authorized: clear the pending request
    crate::db::gitlab::clear_comment_merge(domain, project_id, mr_iid, db_pool).await?;

    Ok(())
}

/// Handle a merge accept command
#[allow(clippy::too_many_arguments)]
async fn handle_merge_accept(
    client: &GitLabClient,
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    note_id: i64,
    requester_id: i64,
    requester_username: &str,
    method_str: Option<&str>,
    note_created_at: &chrono::DateTime<chrono::Utc>,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    gitlab_sender: &tokio::sync::mpsc::Sender<GitLabTask>,
) -> Result<()> {
    match authorize_commenter(
        client,
        project_id,
        mr_iid,
        requester_id,
        requester_username,
        db_pool,
    )
    .await?
    {
        Authorization::Granted { .. } => {},
        Authorization::Abort => return Ok(()),
        Authorization::Denied => {
            info!(
                "Denying @eka-ci merge from {} on MR !{}: no project write and not a maintainer \
                 of all changed packages",
                requester_username, mr_iid
            );
            if let Err(e) = client
                .create_merge_request_note(
                    project_id,
                    mr_iid,
                    &format!(
                        "@{} I can't merge this MR — you need write access to the project or be a \
                         maintainer of all affected packages.",
                        requester_username
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
    let Some(mr) =
        crate::db::gitlab::get_merge_request_row(domain, project_id, mr_iid, db_pool).await?
    else {
        warn!(
            "MR !{} in project {} not found when processing merge command",
            mr_iid, project_id
        );
        return Ok(());
    };

    if !check_push_timing(
        client,
        project_id,
        mr_iid,
        requester_username,
        &mr.head_sha,
        note_created_at,
    )
    .await?
    {
        return Ok(());
    }

    let rows = crate::db::gitlab::set_comment_merge(
        domain,
        project_id,
        mr_iid,
        &mr.head_sha,
        method_str,
        requester_id,
        requester_username,
        note_id,
        db_pool,
    )
    .await?;

    if rows == 0 {
        warn!(
            "set_comment_merge affected 0 rows for MR !{} in project {}",
            mr_iid, project_id
        );
        return Ok(());
    }

    // Fire the evaluator in case gates are already green
    if let Err(e) = gitlab_sender
        .send(GitLabTask::CheckAutoMerge {
            domain: domain.to_string(),
            project_id,
            mr_iid,
        })
        .await
    {
        warn!("Failed to send CheckAutoMerge task: {:?}", e);
    }

    Ok(())
}

/// Best-effort force-push detection
async fn check_push_timing(
    client: &GitLabClient,
    project_id: i64,
    mr_iid: i64,
    requester_username: &str,
    head_sha: &str,
    note_created_at: &chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    const PUSH_GRACE: chrono::Duration = chrono::Duration::seconds(30);

    match crate::gitlab::actions::fetch_head_commit_date(client, project_id, head_sha).await {
        Ok(Some(commit_date)) if commit_date > *note_created_at + PUSH_GRACE => {
            info!(
                "Refusing @eka-ci merge from {} on MR !{}: head commit {} committed at {} is \
                 newer than the command comment at {} (grace={}s); likely post-command push",
                requester_username,
                mr_iid,
                head_sha,
                commit_date,
                note_created_at,
                PUSH_GRACE.num_seconds()
            );
            if let Err(e) = client
                .create_merge_request_note(
                    project_id,
                    mr_iid,
                    &format!(
                        "@{} I can't merge this MR — the head commit (`{}`) appears to have been \
                         pushed after your `@eka-ci merge` command. Please review the latest \
                         changes and re-issue the command if you still want to merge.",
                        requester_username,
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
