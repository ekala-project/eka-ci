use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::git::GitTask;
use crate::gitlab::GitLabTask;

/// Handle GitLab webhook payload
///
/// GitLab webhooks use a different event model than GitHub:
/// - Uses X-Gitlab-Event header instead of X-GitHub-Event
/// - Different payload structures for MRs, pipelines, etc.
/// - Uses X-Gitlab-Token for webhook verification
pub async fn handle_webhook_payload(
    event_type: &str,
    _payload: serde_json::Value,
    _git_sender: mpsc::Sender<GitTask>,
    _gitlab_sender: mpsc::Sender<GitLabTask>,
    _db_service: DbService,
) {
    debug!("Received GitLab webhook event: {}", event_type);

    match event_type {
        "Merge Request Hook" => {
            debug!("GitLab merge request webhook (not yet implemented)");
            // TODO: Parse MR payload and create GitLabTask::CreateJobSet
        },
        "Push Hook" => {
            debug!("GitLab push webhook (not yet implemented)");
            // TODO: Handle push events for main branch builds
        },
        "Note Hook" => {
            debug!("GitLab note (comment) webhook (not yet implemented)");
            // TODO: Handle comment commands like merge requests
        },
        "Pipeline Hook" => {
            debug!("GitLab pipeline webhook (ignoring)");
            // We don't need to react to our own pipeline events
        },
        _ => {
            warn!("Unhandled GitLab webhook event type: {}", event_type);
        },
    }

    // TODO: Implement full webhook handling:
    // 1. Parse event-specific payloads
    // 2. Extract project/MR metadata
    // 3. Send appropriate GitLabTask messages
    // 4. Handle comment-based merge commands
}
