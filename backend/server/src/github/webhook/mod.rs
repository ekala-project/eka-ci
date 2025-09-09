use octocrab::models::webhook_events::{payload, WebhookEventPayload as WEP};

pub async fn handle_webhook_payload(webhook_payload: WEP) {
    match webhook_payload {
        WEP::PullRequest(pr) => {handle_github_pr(*pr).await},
        _ => return,
    };
    return
}

async fn handle_github_pr(pr: payload::PullRequestWebhookEventPayload) {
    return;
}

