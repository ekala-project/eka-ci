use octocrab::models::pulls::PullRequest;
use octocrab::models::webhook_events::{WebhookEventPayload as WEP, payload};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::git::GitTask;
use crate::github::{CICheckInfo, GitHubTask};

pub async fn handle_webhook_payload(
    webhook_payload: WEP,
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
) {
    match webhook_payload {
        WEP::PullRequest(pr) => handle_github_pr(*pr, git_sender, github_sender).await,
        // We probably don't want to react to every push
        // WEP::Push(pr) => handle_github_push(*pr).await,
        _ => return,
    };
    return;
}

async fn handle_github_pr(
    pr: payload::PullRequestWebhookEventPayload,
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
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
            debug!(
                "Received close/draft event for PR #{}, cancelling check runs",
                pr.pull_request.number
            );

            if let Some(sender) = github_sender {
                let ci_check_info = CICheckInfo::from_gh_pr_head(&pr.pull_request);

                if let Err(e) = sender
                    .send(GitHubTask::CancelCheckRunsForCommit { ci_check_info })
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
            } else {
                debug!("GitHub service not available, skipping cancellation");
            }
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
