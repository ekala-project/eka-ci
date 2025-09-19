use tracing::debug;
use octocrab::models::webhook_events::{payload, WebhookEventPayload as WEP};
use octocrab::models::pulls::PullRequest;

pub async fn handle_webhook_payload(webhook_payload: WEP) {
    match webhook_payload {
        WEP::PullRequest(pr) => {handle_github_pr(*pr).await},
        WEP::Push(pr) => {handle_github_push(*pr).await},
        _ => return,
    };
    return
}

async fn handle_github_pr(pr: payload::PullRequestWebhookEventPayload) {
    use payload::PullRequestWebhookEventAction as PRWEA;
    match pr.action {
        PRWEA::Opened | PRWEA::Edited | PRWEA::Enqueued | PRWEA::Reopened => {
            // TODO launch PR workflow
            todo!()
        },
        PRWEA::Closed | PRWEA::ConvertedToDraft => {
            // TODO, should be safe to cancel any in progress work
            todo!()
        },
        action => {
            debug!("Ignoring non-actionable PR action: {:?}", &action);
        }

    }
}

// base_ref is optional, so may actually be hard to determine what we can "diff"
// the evaluation from
async fn handle_github_push(push: payload::PushWebhookEventPayload) {
    debug!("Received github push notification: {:?}", push.head_commit);
    return;
}

/// This is needed to checkout potentially private github repos
/// These tokens only last an hour
async fn fetch_installation_token(pr: PullRequest) {

}
async fn eval_pr(pr: PullRequest) {

}
