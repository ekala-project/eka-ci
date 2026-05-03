// Auto-merge evaluation and execution for GitLab

pub(crate) mod commands;

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::gitlab::GitLabClient;
use crate::gitlab::types::GitLabTask;

/// Check if an MR is eligible for auto-merge and execute if ready
pub(super) async fn handle_check_auto_merge(
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    client: &GitLabClient,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    gitlab_sender: &tokio::sync::mpsc::Sender<GitLabTask>,
) -> Result<()> {
    info!(
        "Checking auto-merge eligibility for MR !{} in project {} on {}",
        mr_iid, project_id, domain
    );

    // Defer until head-commit jobset has fully succeeded
    if !crate::db::gitlab::mr_head_build_succeeded(domain, project_id, mr_iid, db_pool).await? {
        info!(
            "MR !{} head build not yet successful, deferring auto-merge",
            mr_iid
        );
        return Ok(());
    };

    // Look up MR by (domain, project_id, mr_iid)
    let Some(mr) =
        crate::db::gitlab::get_merge_request_row(domain, project_id, mr_iid, db_pool).await?
    else {
        warn!(
            "MR !{} not found in project {} on {} while evaluating auto-merge",
            mr_iid, project_id, domain
        );
        return Ok(());
    };

    let pending_cmt_merge = mr.pending_comment_merge();

    // SHA-drift check: comment-merges are pinned to a commit
    if let Some(cmr) = pending_cmt_merge.as_ref() {
        if cmr.sha != mr.head_sha {
            cancel_drifted_comment_merge(
                domain,
                project_id,
                mr_iid,
                cmr,
                &mr.head_sha,
                gitlab_sender,
                db_pool,
            )
            .await?;
            return Ok(());
        }
    }

    // At least one merge path must be active
    if !mr.auto_merge_enabled && pending_cmt_merge.is_none() {
        debug!(
            "MR !{} in project {} on {} has no active auto-merge or comment-merge request; \
             skipping",
            mr_iid, project_id, domain
        );
        return Ok(());
    }

    let changed_packages =
        crate::db::gitlab::get_mr_changed_packages(domain, project_id, mr_iid, db_pool).await?;

    if changed_packages.is_empty() {
        info!(
            "MR !{} has no changed packages, skipping auto-merge",
            mr_iid
        );
        return Ok(());
    }

    // Maintainer-approval gate. Skipped for comment-driven merges
    if pending_cmt_merge.is_none() {
        let (eligible, missing_approvals) = crate::gitlab::actions::check_mr_maintainer_approvals(
            client,
            project_id,
            mr_iid,
            &changed_packages,
            db_pool,
        )
        .await?;

        if !eligible {
            info!(
                "MR !{} is not eligible for auto-merge. Missing approvals for packages: {:?}",
                mr_iid, missing_approvals
            );
            return Ok(());
        }
    }

    // Method: comment request → MR-stored preference → "merge"
    let merge_method = pending_cmt_merge
        .as_ref()
        .and_then(|cmr| cmr.method.as_deref())
        .or(mr.merge_method.as_deref())
        .unwrap_or("merge");

    // Validate against project settings before trying
    match crate::gitlab::actions::validate_merge_method(client, project_id, merge_method).await {
        Ok(crate::gitlab::actions::MergeMethodCheck::Ok) => {},
        Ok(crate::gitlab::actions::MergeMethodCheck::NotAllowed { allowed }) => {
            warn!(
                "MR !{} in project {} on {}: configured merge method '{}' is not allowed by \
                 project settings (allowed: {:?}); skipping auto-merge",
                mr_iid, project_id, domain, merge_method, allowed
            );
            return Ok(());
        },
        Err(e) => {
            warn!(
                "MR !{} in project {} on {}: failed to fetch project merge settings: {:?}; \
                 skipping auto-merge",
                mr_iid, project_id, domain, e
            );
            return Ok(());
        },
    }

    auto_merge_execute(
        client,
        domain,
        project_id,
        mr_iid,
        merge_method,
        pending_cmt_merge.as_ref(),
        db_pool,
    )
    .await;

    Ok(())
}

/// Notify requester and clear the pending row when a comment-merge's
/// pinned SHA no longer matches the MR head.
async fn cancel_drifted_comment_merge(
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    cmr: &crate::db::gitlab::CommentMergeRequest,
    current_head: &str,
    gitlab_sender: &tokio::sync::mpsc::Sender<GitLabTask>,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
) -> Result<()> {
    warn!(
        "MR !{} in project {} on {}: comment-merge SHA drift (requested {}, now {}); cancelling",
        mr_iid, project_id, domain, cmr.sha, current_head
    );

    // Best-effort notifications
    if let Err(e) = gitlab_sender
        .send(GitLabTask::CommentMergeDriftCancelled {
            domain: domain.to_string(),
            project_id,
            mr_iid,
            expected_sha: cmr.sha.clone(),
            actual_sha: current_head.to_string(),
            requester_username: cmr.requester_username.clone(),
        })
        .await
    {
        warn!("Failed to send CommentMergeDriftCancelled task: {:?}", e);
    }

    crate::db::gitlab::clear_comment_merge(domain, project_id, mr_iid, db_pool).await?;
    Ok(())
}

/// Execute the merge + record post-conditions. Infallible at the
/// caller level — a failed merge is logged and swallowed.
async fn auto_merge_execute(
    client: &GitLabClient,
    domain: &str,
    project_id: i64,
    mr_iid: i64,
    merge_method: &str,
    pending_cmt_merge: Option<&crate::db::gitlab::CommentMergeRequest>,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
) {
    info!(
        "Auto-merging MR !{} in project {} on {} using method '{}'",
        mr_iid, project_id, domain, merge_method
    );

    let request = crate::gitlab::client::MergeMergeRequestRequest {
        merge_commit_message: None,
        squash_commit_message: None,
        should_remove_source_branch: None,
        merge_when_pipeline_succeeds: None,
        sha: None,
    };

    match client
        .merge_merge_request(project_id, mr_iid, request)
        .await
    {
        Ok(_) => {
            info!(
                "Successfully auto-merged MR !{} in project {} on {}",
                mr_iid, project_id, domain
            );

            // Mark as merged in database
            if let Err(e) = crate::db::gitlab::mark_mr_merged(
                domain,
                project_id,
                mr_iid,
                pending_cmt_merge.map(|c| c.requester_id),
                db_pool,
            )
            .await
            {
                warn!(
                    "Failed to mark MR !{} as merged in database: {:?}",
                    mr_iid, e
                );
            }
        },
        Err(e) => {
            warn!(
                "Failed to auto-merge MR !{} in project {} on {}: {:?}",
                mr_iid, project_id, domain, e
            );
        },
    }
}
