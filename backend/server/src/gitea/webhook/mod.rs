use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::git::GitTask;
use crate::gitea::GiteaTask;

/// Handle Gitea webhook payload
///
/// Gitea uses a GitHub-compatible webhook API, but with some differences:
/// - Uses X-Gitea-Event header instead of X-GitHub-Event
/// - Similar but not identical payload structures
/// - Self-hosted instances have domain-specific configuration
/// - Newer versions support check runs, older versions use commit statuses
pub async fn handle_webhook_payload(
    event_type: &str,
    _payload: serde_json::Value,
    _git_sender: mpsc::Sender<GitTask>,
    _gitea_sender: mpsc::Sender<GiteaTask>,
    _db_service: DbService,
) {
    debug!("Received Gitea webhook event: {}", event_type);

    match event_type {
        "pull_request" => {
            debug!("Gitea pull request webhook (not yet implemented)");
            // TODO: Parse PR payload and create GiteaTask::CreateJobSet
        },
        "push" => {
            debug!("Gitea push webhook (not yet implemented)");
            // TODO: Handle push events for main branch builds
        },
        "issue_comment" | "pull_request_comment" => {
            debug!("Gitea comment webhook (not yet implemented)");
            // TODO: Handle comment commands for merge requests
        },
        "pull_request_review" => {
            debug!("Gitea PR review webhook (not yet implemented)");
            // TODO: Handle review approvals for auto-merge
        },
        _ => {
            warn!("Unhandled Gitea webhook event type: {}", event_type);
        },
    }

    // TODO: Implement full webhook handling:
    // 1. Parse event-specific payloads (GitHub-compatible format)
    // 2. Extract repository/PR/domain metadata
    // 3. Detect instance version (check runs vs commit statuses)
    // 4. Send appropriate GiteaTask messages
    // 5. Handle comment-based merge commands
}
