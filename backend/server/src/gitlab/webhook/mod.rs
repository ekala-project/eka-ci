pub mod comment_command;

use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::db::DbService;
use crate::git::GitTask;
use crate::gitlab::GitLabTask;

#[derive(Debug, Deserialize)]
struct MergeRequestPayload {
    object_attributes: MergeRequestAttributes,
    project: Project,
    user: User,
}

#[derive(Debug, Deserialize)]
struct MergeRequestAttributes {
    iid: i64,
    title: String,
    state: String,
    #[serde(rename = "source_branch")]
    source_branch: String,
    #[serde(rename = "target_branch")]
    target_branch: String,
    #[serde(rename = "last_commit")]
    last_commit: Commit,
    action: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NotePayload {
    object_attributes: NoteAttributes,
    merge_request: Option<MergeRequestInfo>,
    project: Project,
    user: User,
}

#[derive(Debug, Deserialize)]
struct NoteAttributes {
    id: i64,
    note: String,
    #[serde(rename = "created_at")]
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct MergeRequestInfo {
    iid: i64,
    #[serde(rename = "last_commit")]
    last_commit: Commit,
}

#[derive(Debug, Deserialize)]
struct Project {
    id: i64,
    #[serde(rename = "path_with_namespace")]
    path_with_namespace: String,
    #[serde(rename = "web_url")]
    web_url: String,
}

#[derive(Debug, Deserialize)]
struct User {
    id: i64,
    username: String,
}

#[derive(Debug, Deserialize)]
struct Commit {
    id: String,
}

/// Handle GitLab webhook payload
///
/// GitLab webhooks use a different event model than GitHub:
/// - Uses X-Gitlab-Event header instead of X-GitHub-Event
/// - Different payload structures for MRs, pipelines, etc.
/// - Uses X-Gitlab-Token for webhook verification
pub async fn handle_webhook_payload(
    event_type: &str,
    payload: serde_json::Value,
    _git_sender: mpsc::Sender<GitTask>,
    gitlab_sender: mpsc::Sender<GitLabTask>,
    db_service: DbService,
) {
    debug!("Received GitLab webhook event: {}", event_type);

    match event_type {
        "Merge Request Hook" => {
            if let Err(e) = handle_merge_request_event(payload, gitlab_sender, db_service).await {
                warn!("Failed to handle merge request event: {:?}", e);
            }
        },
        "Push Hook" => {
            debug!("GitLab push webhook (not yet implemented)");
            // TODO: Handle push events for main branch builds
        },
        "Note Hook" => {
            if let Err(e) = handle_note_event(payload, gitlab_sender).await {
                warn!("Failed to handle note event: {:?}", e);
            }
        },
        "Pipeline Hook" => {
            debug!("GitLab pipeline webhook (ignoring)");
            // We don't need to react to our own pipeline events
        },
        _ => {
            warn!("Unhandled GitLab webhook event type: {}", event_type);
        },
    }
}

async fn handle_merge_request_event(
    payload: serde_json::Value,
    gitlab_sender: mpsc::Sender<GitLabTask>,
    db_service: DbService,
) -> anyhow::Result<()> {
    let event: MergeRequestPayload = serde_json::from_value(payload)?;

    let domain = extract_domain(&event.project.web_url);
    let project_id = event.project.id;
    let mr_iid = event.object_attributes.iid;

    // Store/update MR in database
    let (owner, repo_name) = parse_path_with_namespace(&event.project.path_with_namespace);

    crate::db::gitlab::upsert_merge_request(
        mr_iid,
        &owner,
        &repo_name,
        project_id,
        &domain,
        &event.object_attributes.last_commit.id,
        &event.object_attributes.target_branch, // Use target branch as base for simplicity
        &event.object_attributes.title,
        &event.user.username,
        &event.object_attributes.state,
        &db_service.pool,
    )
    .await?;

    // Trigger auto-merge check on certain actions
    match event.object_attributes.action.as_deref() {
        Some("update") | Some("open") | Some("reopen") | Some("approved") => {
            info!(
                "Triggering auto-merge check for MR !{} in project {}",
                mr_iid, project_id
            );
            gitlab_sender
                .send(GitLabTask::CheckAutoMerge {
                    domain: domain.to_string(),
                    project_id,
                    mr_iid,
                })
                .await?;
        },
        Some("merge") => {
            debug!("MR !{} was merged, no action needed", mr_iid);
        },
        Some("close") => {
            debug!("MR !{} was closed, no action needed", mr_iid);
        },
        _ => {
            debug!(
                "MR !{} action {:?} doesn't trigger auto-merge check",
                mr_iid, event.object_attributes.action
            );
        },
    }

    Ok(())
}

async fn handle_note_event(
    payload: serde_json::Value,
    gitlab_sender: mpsc::Sender<GitLabTask>,
) -> anyhow::Result<()> {
    let event: NotePayload = serde_json::from_value(payload)?;

    // Only process MR comments
    let Some(mr_info) = event.merge_request else {
        debug!("Note is not on a merge request, ignoring");
        return Ok(());
    };

    // Check if this is a merge command
    if let Some(command) = comment_command::parse_comment_command(&event.object_attributes.note) {
        let domain = extract_domain(&event.project.web_url);

        info!(
            "Detected merge command {:?} from {} on MR !{} in project {}",
            command, event.user.username, mr_info.iid, event.project.id
        );

        // Parse the created_at timestamp
        let note_created_at =
            chrono::DateTime::parse_from_rfc3339(&event.object_attributes.created_at)
                .ok()
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .unwrap_or_else(chrono::Utc::now);

        gitlab_sender
            .send(GitLabTask::ProcessMergeCommand {
                domain: domain.to_string(),
                project_id: event.project.id,
                mr_iid: mr_info.iid,
                note_id: event.object_attributes.id,
                requester_id: event.user.id,
                requester_username: event.user.username,
                body: event.object_attributes.note,
                note_created_at,
            })
            .await?;
    }

    Ok(())
}

/// Extract domain from GitLab web URL
fn extract_domain(web_url: &str) -> String {
    web_url
        .strip_prefix("https://")
        .or_else(|| web_url.strip_prefix("http://"))
        .and_then(|s| s.split('/').next())
        .unwrap_or("gitlab.com")
        .to_string()
}

/// Parse "owner/repo" from path_with_namespace
fn parse_path_with_namespace(path: &str) -> (String, String) {
    let parts: Vec<&str> = path.splitn(2, '/').collect();
    match parts.as_slice() {
        [owner, repo] => (owner.to_string(), repo.to_string()),
        [single] => ("unknown".to_string(), single.to_string()),
        _ => ("unknown".to_string(), "unknown".to_string()),
    }
}
