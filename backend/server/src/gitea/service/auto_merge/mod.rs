// Auto-merge evaluation and execution for Gitea PRs

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::db::DbService;
use crate::gitea::GiteaClient;
use crate::gitea::types::GiteaTask;

pub(in crate::gitea::service) mod commands;

/// Check auto-merge eligibility and execute if ready
pub(super) async fn handle_check_auto_merge(
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    client: &GiteaClient,
    db_service: &DbService,
    gitea_sender: &tokio::sync::mpsc::Sender<GiteaTask>,
) -> Result<()> {
    info!(
        "Checking auto-merge eligibility for PR #{} in {}/{} on {}",
        pr_number, owner, repo_name, domain
    );

    // Defer until head-commit jobset has fully succeeded
    if !crate::db::gitea::pr_head_build_succeeded(
        domain,
        owner,
        repo_name,
        pr_number,
        &db_service.pool,
    )
    .await?
    {
        info!(
            "PR #{} head build not yet successful, deferring auto-merge",
            pr_number
        );
        return Ok(());
    };

    // Look up PR by (domain, owner, repo_name, pr_number)
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
            "PR #{} not found in {}/{} on {} while evaluating auto-merge",
            pr_number, owner, repo_name, domain
        );
        return Ok(());
    };

    let pending_cmt_merge = pr.pending_comment_merge();

    // SHA-drift check: comment-merges are pinned to a commit
    if let Some(cmr) = pending_cmt_merge.as_ref() {
        if cmr.sha != pr.head_sha {
            cancel_drifted_comment_merge(
                domain,
                owner,
                repo_name,
                pr_number,
                cmr,
                &pr.head_sha,
                gitea_sender,
                db_service,
            )
            .await?;
            return Ok(());
        }
    }

    // At least one merge path must be active
    if !pr.auto_merge_enabled && pending_cmt_merge.is_none() {
        debug!(
            "PR #{} in {}/{} on {} has no active auto-merge or comment-merge request; skipping",
            pr_number, owner, repo_name, domain
        );
        return Ok(());
    }

    let changed_packages = crate::db::gitea::get_pr_changed_packages(
        domain,
        owner,
        repo_name,
        pr_number,
        &db_service.pool,
    )
    .await?;

    if changed_packages.is_empty() {
        info!(
            "PR #{} has no changed packages, skipping auto-merge",
            pr_number
        );
        return Ok(());
    }

    // Maintainer-approval gate. Skipped for comment-driven merges
    if pending_cmt_merge.is_none() {
        let (eligible, missing_approvals) = crate::gitea::actions::check_pr_maintainer_approvals(
            client,
            owner,
            repo_name,
            pr_number,
            &changed_packages,
            &db_service.pool,
        )
        .await?;

        if !eligible {
            info!(
                "PR #{} is not eligible for auto-merge. Missing approvals for packages: {:?}",
                pr_number, missing_approvals
            );
            return Ok(());
        }
    }

    // Method: comment request → PR-stored preference → "squash"
    let merge_method = pending_cmt_merge
        .as_ref()
        .and_then(|cmr| cmr.method.as_deref())
        .or(pr.merge_method.as_deref())
        .unwrap_or("squash");

    // Validate against repository settings before trying
    match crate::gitea::actions::validate_merge_method(client, owner, repo_name, merge_method).await
    {
        Ok(crate::gitea::actions::MergeMethodCheck::Ok) => {},
        Ok(crate::gitea::actions::MergeMethodCheck::NotAllowed { allowed }) => {
            warn!(
                "PR #{} in {}/{} on {}: configured merge method '{}' is not allowed by repository \
                 settings (allowed: {:?}); skipping auto-merge",
                pr_number, owner, repo_name, domain, merge_method, allowed
            );
            return Ok(());
        },
        Err(e) => {
            warn!(
                "PR #{} in {}/{} on {}: failed to fetch repository merge settings: {:?}; skipping \
                 auto-merge",
                pr_number, owner, repo_name, domain, e
            );
            return Ok(());
        },
    }

    auto_merge_execute(
        client,
        domain,
        owner,
        repo_name,
        pr_number,
        merge_method,
        pending_cmt_merge.as_ref(),
        db_service,
    )
    .await;

    Ok(())
}

/// Notify requester and clear the pending row when a comment-merge's
/// pinned SHA no longer matches the PR head.
async fn cancel_drifted_comment_merge(
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    cmr: &crate::db::gitea::CommentMergeRequest,
    current_head: &str,
    gitea_sender: &tokio::sync::mpsc::Sender<GiteaTask>,
    db_service: &DbService,
) -> Result<()> {
    warn!(
        "PR #{} in {}/{} on {}: comment-merge SHA drift (requested {}, now {}); cancelling",
        pr_number, owner, repo_name, domain, cmr.sha, current_head
    );

    // Best-effort notifications
    if let Err(e) = gitea_sender
        .send(GiteaTask::CommentMergeDriftCancelled {
            domain: domain.to_string(),
            owner: owner.to_string(),
            repo_name: repo_name.to_string(),
            pr_number,
            expected_sha: cmr.sha.clone(),
            actual_sha: current_head.to_string(),
            requester_login: cmr.requester_login.clone(),
        })
        .await
    {
        warn!("Failed to send CommentMergeDriftCancelled task: {:?}", e);
    }

    crate::db::gitea::clear_comment_merge(domain, owner, repo_name, pr_number, &db_service.pool)
        .await?;
    Ok(())
}

/// Execute the merge + record post-conditions. Infallible at the
/// caller level — a failed merge is logged and swallowed.
async fn auto_merge_execute(
    client: &GiteaClient,
    domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    merge_method: &str,
    pending_cmt_merge: Option<&crate::db::gitea::CommentMergeRequest>,
    db_service: &DbService,
) {
    info!(
        "Auto-merging PR #{} in {}/{} on {} using method '{}'",
        pr_number, owner, repo_name, domain, merge_method
    );

    let merge_request = crate::gitea::client::MergePullRequestRequest {
        merge_method: merge_method.to_string(),
        merge_message_field: None,
        merge_title_field: None,
    };

    match client
        .merge_pull_request(owner, repo_name, pr_number, merge_request)
        .await
    {
        Ok(_) => {
            info!(
                "Successfully auto-merged PR #{} in {}/{} on {}",
                pr_number, owner, repo_name, domain
            );

            // Mark as merged in database
            if let Err(e) = crate::db::gitea::mark_pr_merged(
                domain,
                owner,
                repo_name,
                pr_number,
                pending_cmt_merge.map(|c| c.requester_id),
                &db_service.pool,
            )
            .await
            {
                warn!(
                    "Failed to mark PR #{} as merged in database: {:?}",
                    pr_number, e
                );
            }
        },
        Err(e) => {
            warn!(
                "Failed to auto-merge PR #{} in {}/{} on {}: {:?}",
                pr_number, owner, repo_name, domain, e
            );
        },
    }
}

/// Handle comment merge drift cancelled notification
pub(super) async fn handle_comment_merge_drift_cancelled(
    _domain: &str,
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    expected_sha: &str,
    actual_sha: &str,
    requester_login: &str,
    client: &GiteaClient,
) -> Result<()> {
    use crate::gitea::service::helpers::short_sha;

    let body = format!(
        "@{} your `@eka-ci merge` request was cancelled because new commits landed on this PR \
         since you issued the command.\n\n- expected head: `{}`\n- current head: `{}`\n\nIf you \
         still want to merge, re-issue `@eka-ci merge` on the updated PR.",
        requester_login,
        short_sha(expected_sha),
        short_sha(actual_sha),
    );

    if let Err(e) = client
        .create_issue_comment(owner, repo_name, pr_number, &body)
        .await
    {
        warn!(
            "Failed to post SHA-drift comment on PR #{} in {}/{}: {:?}",
            pr_number, owner, repo_name, e
        );
    }
    Ok(())
}
