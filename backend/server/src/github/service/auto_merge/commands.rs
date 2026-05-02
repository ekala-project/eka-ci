// Comment-driven merge command processing

use anyhow::Result;
use octocrab::Octocrab;
use tracing::{debug, info, warn};

use super::super::GitHubService;
use super::auth::Authorization;
use crate::github::service::{GitHubTask, actions};

impl GitHubService {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::github::service) async fn handle_process_merge_command(
        &self,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        comment_id: i64,
        requester_id: i64,
        requester_login: &str,
        body: &str,
        comment_created_at: &chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        use crate::github::webhook::comment_command::{CommentCommand, parse_comment_command};

        let octocrab = self.octocrab_for_owner(owner)?;

        // Re-parse rather than carrying a typed command — payload stays
        // POD and parsing stays in one place.
        let Some(cmd) = parse_comment_command(body) else {
            debug!(
                "Comment {} on {}/{}#{} no longer parses as a command; dropping",
                comment_id, owner, repo_name, pr_number
            );
            return Ok(());
        };

        match cmd {
            CommentCommand::MergeCancel => {
                self.handle_merge_cancel(
                    &octocrab,
                    owner,
                    repo_name,
                    pr_number,
                    comment_id,
                    requester_id,
                    requester_login,
                )
                .await
            },
            CommentCommand::Merge { method } => {
                self.handle_merge_accept(
                    &octocrab,
                    owner,
                    repo_name,
                    pr_number,
                    comment_id,
                    requester_id,
                    requester_login,
                    method.as_ref().map(|m| m.as_str()),
                    comment_created_at,
                )
                .await
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_merge_cancel(
        &self,
        octocrab: &Octocrab,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        comment_id: i64,
        requester_id: i64,
        requester_login: &str,
    ) -> Result<()> {
        // Silent no-op when nothing is pending so unauthorized commenters
        // learn no bot state.
        let pr_row = crate::db::github::get_pull_request_row(
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?;
        let Some(pending) = pr_row.as_ref().and_then(|p| p.pending_comment_merge()) else {
            debug!(
                "No pending comment-merge on {}/{}#{}; ignoring cancel from {}",
                owner, repo_name, pr_number, requester_login
            );
            return Ok(());
        };

        let is_self = requester_id == pending.requester_id;

        // Self-cancel fast path: skip API permission lookup.
        let authorized = if is_self {
            true
        } else {
            match self
                .authorize_commenter(
                    octocrab,
                    owner,
                    repo_name,
                    pr_number,
                    requester_id,
                    requester_login,
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
                "Denying @eka-ci merge cancel from {} on {}/{}#{}: not the original requester, no \
                 repo write, and not a maintainer of all changed packages",
                requester_login, owner, repo_name, pr_number
            );
            if let Err(e) =
                actions::add_comment_reaction(octocrab, owner, repo_name, comment_id, "-1").await
            {
                warn!("Failed to react to denied merge-cancel: {:?}", e);
            }
            if let Err(e) = actions::post_issue_comment(
                octocrab,
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

        // Authorized: clear the pending request and ack.
        crate::db::github::clear_comment_merge_request(
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?;
        if let Err(e) =
            actions::add_comment_reaction(octocrab, owner, repo_name, comment_id, "+1").await
        {
            warn!(
                "Failed to ack merge-cancel on comment {}: {:?}",
                comment_id, e
            );
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_merge_accept(
        &self,
        octocrab: &Octocrab,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        comment_id: i64,
        requester_id: i64,
        requester_login: &str,
        method_str: Option<&str>,
        comment_created_at: &chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        match self
            .authorize_commenter(
                octocrab,
                owner,
                repo_name,
                pr_number,
                requester_id,
                requester_login,
            )
            .await?
        {
            Authorization::Granted { .. } => {},
            Authorization::Abort => return Ok(()),
            Authorization::Denied => {
                info!(
                    "Denying @eka-ci merge from {} on {}/{}#{}: no repo write and not a \
                     maintainer of all changed packages",
                    requester_login, owner, repo_name, pr_number
                );
                if let Err(e) =
                    actions::add_comment_reaction(octocrab, owner, repo_name, comment_id, "-1")
                        .await
                {
                    warn!("Failed to react to denied merge: {:?}", e);
                }
                if let Err(e) = actions::post_issue_comment(
                    octocrab,
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

        // Pin the merge to the current head SHA.
        let Some(pr) = crate::db::github::get_pull_request_row(
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?
        else {
            warn!(
                "PR {}/{}#{} not found when processing merge command",
                owner, repo_name, pr_number
            );
            return Ok(());
        };

        if !self
            .check_push_timing(
                octocrab,
                owner,
                repo_name,
                pr_number,
                comment_id,
                requester_login,
                &pr.head_sha,
                comment_created_at,
            )
            .await?
        {
            return Ok(());
        }

        let rows = crate::db::github::set_comment_merge_request(
            owner,
            repo_name,
            pr_number,
            &pr.head_sha,
            method_str,
            requester_id,
            requester_login,
            comment_id,
            &self.db_service.pool,
        )
        .await?;
        if rows == 0 {
            warn!(
                "set_comment_merge_request affected 0 rows for {}/{}#{}",
                owner, repo_name, pr_number
            );
            return Ok(());
        }

        // Ack; the actual merge fires via the auto-merge evaluator.
        if let Err(e) =
            actions::add_comment_reaction(octocrab, owner, repo_name, comment_id, "+1").await
        {
            warn!("Failed to ack merge command: {:?}", e);
        }

        // Fire the evaluator in case gates are already green.
        if let Err(e) = self
            .github_sender
            .send(GitHubTask::CheckAutoMerge {
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

    /// Best-effort force-push detection. Returns `Ok(true)` to proceed,
    /// `Ok(false)` to refuse (caller must return early). API errors and
    /// missing dates fall through to accept — the post-acceptance drift
    /// hook is the backstop. 30s grace is deliberately tight.
    #[allow(clippy::too_many_arguments)]
    async fn check_push_timing(
        &self,
        octocrab: &Octocrab,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        comment_id: i64,
        requester_login: &str,
        head_sha: &str,
        comment_created_at: &chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        use super::short_sha;

        const PUSH_GRACE: chrono::Duration = chrono::Duration::seconds(30);
        match actions::fetch_head_commit_date(octocrab, owner, repo_name, head_sha).await {
            Ok(Some(commit_date)) if commit_date > *comment_created_at + PUSH_GRACE => {
                info!(
                    "Refusing @eka-ci merge from {} on {}/{}#{}: head commit {} committed at {} \
                     is newer than the command comment at {} (grace={}s); likely post-command push",
                    requester_login,
                    owner,
                    repo_name,
                    pr_number,
                    head_sha,
                    commit_date,
                    comment_created_at,
                    PUSH_GRACE.num_seconds()
                );
                if let Err(e) =
                    actions::add_comment_reaction(octocrab, owner, repo_name, comment_id, "-1")
                        .await
                {
                    warn!("Failed to react to refused merge (push drift): {:?}", e);
                }
                if let Err(e) = actions::post_issue_comment(
                    octocrab,
                    owner,
                    repo_name,
                    pr_number,
                    &format!(
                        "@{} I can't merge this PR — the head commit (`{}`) appears to have been \
                         pushed after your `@eka-ci merge` command. Please review the latest \
                         changes and re-issue the command if you still want to merge.",
                        requester_login,
                        short_sha(head_sha)
                    ),
                )
                .await
                {
                    warn!("Failed to post push drift comment: {:?}", e);
                }
                Ok(false)
            },
            Ok(Some(_)) => Ok(true), // commit predates the comment
            Ok(None) => {
                warn!(
                    "Head commit {}@{}/{} has no parseable committer date; proceeding without \
                     push-time check (best effort)",
                    head_sha, owner, repo_name
                );
                Ok(true)
            },
            Err(e) => {
                warn!(
                    "Failed to fetch head commit date for {}/{}@{}: {:?}; proceeding without \
                     push-time check (best effort)",
                    owner, repo_name, head_sha, e
                );
                Ok(true)
            },
        }
    }
}
