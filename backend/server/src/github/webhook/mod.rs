use octocrab::models::pulls::PullRequest;
use octocrab::models::webhook_events::{WebhookEventPayload as WEP, payload};
use tracing::debug;

pub async fn handle_webhook_payload(webhook_payload: WEP) {
    match webhook_payload {
        WEP::PullRequest(pr) => handle_github_pr(*pr).await,
        // We probably don't want to react to every push
        // WEP::Push(pr) => handle_github_push(*pr).await,
        _ => return,
    };
    return;
}

async fn handle_github_pr(pr: payload::PullRequestWebhookEventPayload) {
    use payload::PullRequestWebhookEventAction as PRWEA;
    match pr.action {
        PRWEA::Opened | PRWEA::Synchronize | PRWEA::Reopened => {
            fetch_installation_token(&pr.pull_request).await;
            // TODO launch PR workflow
            todo!()
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

// base_ref is optional, so may actually be hard to determine what we can "diff"
// the evaluation from
async fn handle_github_push(push: payload::PushWebhookEventPayload) {
    debug!("Received github push notification: {:?}", push.head_commit);
}

/// This is needed to checkout potentially private github repos
/// These tokens only last an hour
async fn fetch_installation_token(pr: &PullRequest) {}
async fn eval_pr(pr: PullRequest) {}
