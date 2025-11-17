use octocrab::models::pulls::PullRequest;
use octocrab::models::webhook_events::{WebhookEventPayload as WEP, payload};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::git::GitTask;

pub async fn handle_webhook_payload(webhook_payload: WEP, git_sender: mpsc::Sender<GitTask>) {
    match webhook_payload {
        WEP::PullRequest(pr) => handle_github_pr(*pr, git_sender).await,
        // We probably don't want to react to every push
        // WEP::Push(pr) => handle_github_push(*pr).await,
        _ => return,
    };
    return;
}

async fn handle_github_pr(
    pr: payload::PullRequestWebhookEventPayload,
    git_sender: mpsc::Sender<GitTask>,
) {
    use payload::PullRequestWebhookEventAction as PRWEA;
    match pr.action {
        PRWEA::Opened | PRWEA::Synchronize | PRWEA::Reopened => {
            debug!("Received event for PR #{}", &pr.pull_request.number);

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
            // TODO, should be safe to cancel any in progress work
            todo!()
        },
        PRWEA::Enqueued => {
            // TODO: Determine what to do for merge queues. If ever relevant
        },
        action => {
            debug!("Ignoring non-actionable PR action: {:?}", &action);
        },
    }
}

#[allow(dead_code)]
// base_ref is optional, so may actually be hard to determine what we can "diff"
// the evaluation from
async fn handle_github_push(push: payload::PushWebhookEventPayload) {
    debug!("Received github push notification: {:?}", push.head_commit);
}

#[allow(dead_code)]
async fn eval_pr(_pr: PullRequest) {}
