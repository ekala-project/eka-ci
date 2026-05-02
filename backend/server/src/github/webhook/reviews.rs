// Pull request review webhook handling

use octocrab::models::webhook_events::payload;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::github::GitHubTask;

/// Handle `pull_request_review` webhook events.
///
/// When a reviewer submits an approval (or dismisses/changes an existing review),
/// re-check auto-merge eligibility for the PR. This ensures approvals that arrive
/// after builds have completed still trigger the merge flow, rather than waiting
/// for a subsequent build to reach the recorder's end-of-jobset hook.
pub(super) async fn handle_github_pr_review(
    review_event: payload::PullRequestReviewWebhookEventPayload,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    db_service: DbService,
) {
    use octocrab::models::pulls::ReviewState;
    use payload::PullRequestReviewWebhookEventAction as PRRWEA;

    let Some(github_sender) = github_sender else {
        warn!("GitHub service is down, unable to service review webhook. Restart Eka-CI");
        return;
    };

    // Only react to events that actually change approval state. Edits to a
    // review body do not change whether the PR is approved.
    match review_event.action {
        PRRWEA::Submitted => {
            // Only approving reviews affect merge eligibility in a positive
            // direction. Non-approving reviews (Commented, ChangesRequested)
            // would only ever prevent a merge, which the existing approval
            // check already handles at merge time, so we skip re-triggering.
            if review_event.review.state != Some(ReviewState::Approved) {
                debug!(
                    "Ignoring non-approving review on PR #{}",
                    review_event.pull_request.number
                );
                return;
            }
        },
        PRRWEA::Dismissed => {
            // A dismissal can affect later recomputation; re-run the check so
            // any cached state is refreshed.
        },
        action => {
            debug!("Ignoring pull_request_review action: {:?}", action);
            return;
        },
    }

    let pr = &review_event.pull_request;
    let pr_number = pr.number as i64;

    let Some(base_repo) = pr.base.repo.as_ref() else {
        warn!(
            "pull_request_review for PR #{} missing base repo, cannot check auto-merge",
            pr_number
        );
        return;
    };

    let Some(owner) = base_repo.owner.as_ref().map(|o| o.login.clone()) else {
        warn!(
            "pull_request_review for PR #{} missing base repo owner, cannot check auto-merge",
            pr_number
        );
        return;
    };
    let repo_name = base_repo.name.clone();

    // Only consider PRs where auto-merge has been enabled and which are still
    // open. This matches the gate applied by the scheduler-driven path.
    let pr_record = match crate::db::github::get_pr_by_head_sha(
        &pr.head.sha,
        &owner,
        &repo_name,
        &db_service.pool,
    )
    .await
    {
        Ok(Some(record)) => record,
        Ok(None) => {
            debug!(
                "No stored PR record for {}/{}#{} (head {}); skipping review-triggered auto-merge",
                owner, repo_name, pr_number, pr.head.sha
            );
            return;
        },
        Err(e) => {
            warn!(
                "Failed to look up PR record for {}/{}#{}: {:?}",
                owner, repo_name, pr_number, e
            );
            return;
        },
    };

    if !pr_record.auto_merge_enabled || pr_record.state != "open" {
        debug!(
            "PR #{} auto-merge not enabled or not open (enabled={}, state={}); skipping",
            pr_number, pr_record.auto_merge_enabled, pr_record.state
        );
        return;
    }

    let task = GitHubTask::CheckAutoMerge {
        owner: owner.clone(),
        repo_name: repo_name.clone(),
        pr_number,
    };

    if let Err(e) = github_sender.send(task).await {
        warn!(
            "Failed to send CheckAutoMerge for PR #{} after review: {:?}",
            pr_number, e
        );
    } else {
        debug!(
            "Queued CheckAutoMerge for PR #{} in {}/{} after review event",
            pr_number, owner, repo_name
        );
    }
}
