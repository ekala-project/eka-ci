// Core auto-merge evaluation and execution

pub mod auth;
pub mod commands;

use anyhow::Result;
use octocrab::Octocrab;
use tracing::{debug, info, warn};

use super::GitHubService;
use crate::github::service::{GitHubTask, actions};

/// Abbreviate a SHA to 7 chars; shorter inputs are returned unchanged.
fn short_sha(sha: &str) -> &str {
    if sha.len() >= 7 { &sha[..7] } else { sha }
}

impl GitHubService {
    pub(in crate::github::service) async fn handle_comment_merge_drift_cancelled(
        &self,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        expected_sha: &str,
        actual_sha: &str,
        requester_login: &str,
    ) -> Result<()> {
        let octocrab = self.octocrab_for_owner(owner)?;
        let body = format!(
            "@{} your `@eka-ci merge` request was cancelled because new commits landed on this PR \
             since you issued the command.\n\n- expected head: `{}`\n- current head: `{}`\n\nIf \
             you still want to merge, re-issue `@eka-ci merge` on the updated PR.",
            requester_login,
            short_sha(expected_sha),
            short_sha(actual_sha),
        );
        if let Err(e) =
            actions::post_issue_comment(&octocrab, owner, repo_name, pr_number, &body).await
        {
            warn!(
                "Failed to post SHA-drift comment on {}/{}#{}: {:?}",
                owner, repo_name, pr_number, e
            );
        }
        Ok(())
    }

    // ---- Auto-merge evaluator ----

    pub(in crate::github::service) async fn handle_check_auto_merge(
        &self,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
    ) -> Result<()> {
        let octocrab = self.octocrab_for_owner(owner)?;

        info!(
            "Checking auto-merge eligibility for PR #{} in {}/{}",
            pr_number, owner, repo_name
        );

        // Defer until head-commit jobset has fully succeeded. Idempotent
        // with the scheduler-driven trigger; defends the review-driven
        // path from merging in-flight or already-failed builds.
        if !crate::db::github::pr_head_build_succeeded(
            pr_number,
            owner,
            repo_name,
            &self.db_service.pool,
        )
        .await?
        {
            info!(
                "PR #{} head build not yet successful, deferring auto-merge",
                pr_number
            );
            return Ok(());
        }

        // Look up by (owner, repo, pr_number) — not head_sha, which
        // may have drifted. Need the raw row for `comment_merge_*`.
        let Some(pr) = crate::db::github::get_pull_request_row(
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?
        else {
            warn!(
                "PR #{} not found in {}/{} while evaluating auto-merge",
                pr_number, owner, repo_name
            );
            return Ok(());
        };

        let pending_cmt_merge = pr.pending_comment_merge();

        // SHA-drift check: comment-merges are pinned to a commit.
        if let Some(cmr) = pending_cmt_merge.as_ref() {
            if cmr.sha != pr.head_sha {
                self.cancel_drifted_comment_merge(owner, repo_name, pr_number, cmr, &pr.head_sha)
                    .await?;
                return Ok(());
            }
        }

        // At least one merge path must be active.
        if !pr.auto_merge_enabled && pending_cmt_merge.is_none() {
            debug!(
                "PR #{} in {}/{} has no active auto-merge or comment-merge request; skipping",
                pr_number, owner, repo_name
            );
            return Ok(());
        }

        let changed_packages = crate::db::github::get_pr_changed_packages(
            pr_number,
            owner,
            repo_name,
            &self.db_service.pool,
        )
        .await?;

        if changed_packages.is_empty() {
            info!(
                "PR #{} has no changed packages, skipping auto-merge",
                pr_number
            );
            return Ok(());
        }

        // Maintainer-approval gate. Skipped for comment-driven merges —
        // authority was verified at ProcessMergeCommand time. UI
        // auto-merge still requires per-package approvals.
        if pending_cmt_merge.is_none() {
            let (eligible, missing_approvals) = actions::check_pr_maintainer_approvals(
                &octocrab,
                owner,
                repo_name,
                pr_number as u64,
                &changed_packages,
                &self.db_service.pool,
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

        // Method: comment request → PR-stored preference → squash.
        let merge_method = pending_cmt_merge
            .as_ref()
            .and_then(|cmr| cmr.method.as_deref())
            .or(pr.merge_method.as_deref())
            .unwrap_or("squash");

        // Validate against repo settings before trying.
        match actions::validate_merge_method(&octocrab, owner, repo_name, merge_method).await {
            Ok(actions::MergeMethodCheck::Ok) => {},
            Ok(actions::MergeMethodCheck::NotAllowed { allowed }) => {
                warn!(
                    "PR #{} in {}/{}: configured merge method '{}' is not allowed by repository \
                     settings (allowed: {:?}); skipping auto-merge",
                    pr_number, owner, repo_name, merge_method, allowed
                );
                return Ok(());
            },
            Err(e) => {
                warn!(
                    "PR #{} in {}/{}: failed to fetch repository merge settings: {:?}; skipping \
                     auto-merge",
                    pr_number, owner, repo_name, e
                );
                return Ok(());
            },
        }

        self.auto_merge_execute(
            &octocrab,
            owner,
            repo_name,
            pr_number,
            merge_method,
            pending_cmt_merge.as_ref(),
        )
        .await;

        Ok(())
    }

    /// Notify requester, react `:confused:`, and clear the pending row
    /// when a comment-merge's pinned SHA no longer matches the PR head.
    async fn cancel_drifted_comment_merge(
        &self,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        cmr: &crate::db::github::CommentMergeRequest,
        current_head: &str,
    ) -> Result<()> {
        warn!(
            "PR #{} in {}/{}: comment-merge SHA drift (requested {}, now {}); cancelling",
            pr_number, owner, repo_name, cmr.sha, current_head
        );

        // Best-effort notifications
        if let Err(e) = self
            .github_sender
            .send(GitHubTask::CommentMergeDriftCancelled {
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
        if let Err(e) = self
            .github_sender
            .send(GitHubTask::ReactToComment {
                owner: owner.to_string(),
                repo_name: repo_name.to_string(),
                comment_id: cmr.comment_id,
                content: "confused",
            })
            .await
        {
            warn!("Failed to send ReactToComment task: {:?}", e);
        }

        crate::db::github::clear_comment_merge_request(
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?;
        Ok(())
    }

    /// Execute the merge + record post-conditions. Infallible at the
    /// caller level — a failed merge is logged and swallowed (the next
    /// trigger will retry).
    async fn auto_merge_execute(
        &self,
        octocrab: &Octocrab,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        merge_method: &str,
        pending_cmt_merge: Option<&crate::db::github::CommentMergeRequest>,
    ) {
        info!(
            "Auto-merging PR #{} in {}/{} using method '{}'",
            pr_number, owner, repo_name, merge_method
        );

        match actions::merge_pull_request(
            octocrab,
            owner,
            repo_name,
            pr_number as u64,
            merge_method,
            None,
            None,
        )
        .await
        {
            Ok(_) => {
                info!(
                    "Successfully auto-merged PR #{} in {}/{}",
                    pr_number, owner, repo_name
                );

                // Capture comment_id before `mark_pr_merged` clears the
                // pending row; used below for the rocket ack.
                let pending_comment_id = pending_cmt_merge.map(|c| c.comment_id);

                if let Err(e) = crate::db::github::mark_pr_merged(
                    owner,
                    repo_name,
                    pr_number,
                    pending_cmt_merge.map(|c| c.requester_id),
                    &self.db_service.pool,
                )
                .await
                {
                    warn!(
                        "Failed to mark PR #{} as merged in database: {:?}",
                        pr_number, e
                    );
                }

                if let Some(comment_id) = pending_comment_id {
                    if let Err(e) = self
                        .github_sender
                        .send(GitHubTask::ReactToComment {
                            owner: owner.to_string(),
                            repo_name: repo_name.to_string(),
                            comment_id,
                            content: "rocket",
                        })
                        .await
                    {
                        warn!("Failed to send ReactToComment task: {:?}", e);
                    }
                }
            },
            Err(e) => {
                warn!(
                    "Failed to auto-merge PR #{} in {}/{}: {:?}",
                    pr_number, owner, repo_name, e
                );
            },
        }
    }
}
