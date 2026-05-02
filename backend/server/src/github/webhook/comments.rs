// Issue comment webhook handling

use octocrab::models::webhook_events::payload;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::comment_command;
use crate::github::GitHubTask;

/// Handle `issue_comment` webhook events.
///
/// GitHub fires this for both plain-issue and PR comments (PRs are issues
/// underneath). We filter to PR comments only, parse the body for an
/// `@eka-ci …` command (via [`comment_command::parse_comment_command`]),
/// and enqueue a `ProcessMergeCommand` for the GitHub service to authorize
/// and action.
///
/// Comments authored by bots (including eka-ci itself) are ignored to
/// prevent feedback loops from our own rocket/+1 reaction flow if we ever
/// start posting parseable bodies.
pub(super) async fn handle_github_issue_comment(
    comment_event: payload::IssueCommentWebhookEventPayload,
    repository_info: Option<(String, String)>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
) {
    use payload::IssueCommentWebhookEventAction as ICWEA;

    let Some(github_sender) = github_sender else {
        warn!("GitHub service is down, unable to service issue_comment webhook. Restart Eka-CI");
        return;
    };

    // Only newly-created comments trigger commands; edit/delete is a no-op.
    if comment_event.action != ICWEA::Created {
        debug!("Ignoring issue_comment action: {:?}", comment_event.action);
        return;
    }

    // `issue.pull_request` is `Some` iff the issue is a PR.
    if comment_event.issue.pull_request.is_none() {
        debug!(
            "Ignoring comment on non-PR issue #{}",
            comment_event.issue.number
        );
        return;
    }

    // Skip bot-authored comments to prevent self-reaction loops.
    if comment_event
        .comment
        .user
        .r#type
        .eq_ignore_ascii_case("Bot")
    {
        debug!(
            "Ignoring bot-authored comment on PR #{}",
            comment_event.issue.number
        );
        return;
    }

    let Some(body) = comment_event.comment.body.as_ref() else {
        debug!(
            "Ignoring empty-body comment on PR #{}",
            comment_event.issue.number
        );
        return;
    };

    // Pre-filter: skip the task queue for the vast majority (non-commands).
    if comment_command::parse_comment_command(body).is_none() {
        return;
    }

    let Some((owner, repo_name)) = repository_info else {
        warn!(
            "issue_comment on PR #{} missing repository info; cannot process command",
            comment_event.issue.number
        );
        return;
    };

    // Per-(user, owner, repo) rate limit. Silent drop on rejection —
    // any feedback would amplify the spam we're containing.
    let requester_id = comment_event.comment.user.id.0 as i64;
    if !comment_command::check_and_record_rate_limit(requester_id, &owner, &repo_name) {
        debug!(
            "Rate-limited @eka-ci command from user {} on {}/{}#{} (comment {})",
            comment_event.comment.user.login,
            owner,
            repo_name,
            comment_event.issue.number,
            comment_event.comment.id.0
        );
        return;
    }

    let task = GitHubTask::ProcessMergeCommand {
        owner,
        repo_name,
        pr_number: comment_event.issue.number as i64,
        comment_id: comment_event.comment.id.0 as i64,
        requester_id,
        requester_login: comment_event.comment.user.login.clone(),
        body: body.clone(),
        comment_created_at: comment_event.comment.created_at,
    };

    if let Err(e) = github_sender.send(task).await {
        warn!(
            "Failed to enqueue ProcessMergeCommand for PR #{}: {:?}",
            comment_event.issue.number, e
        );
    } else {
        debug!(
            "Queued ProcessMergeCommand for PR #{} (comment {})",
            comment_event.issue.number, comment_event.comment.id.0
        );
    }
}
