pub mod comment_command;

use std::collections::HashMap;
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::channels::match_push_channels;
use crate::config::{ChannelConfig, ChannelForge};
use crate::db::DbService;
use crate::git::{GitProtocol, GitRepo, GitTask, GitWorkspace};
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
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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

#[derive(Debug, Deserialize)]
struct PushPayload {
    #[serde(rename = "ref")]
    ref_name: String, // e.g., "refs/heads/main"
    before: String, // SHA before push
    after: String,  // SHA after push
    project: Project,
    #[allow(dead_code)]
    user_name: Option<String>,
    commits: Vec<PushCommit>,
}

#[derive(Debug, Deserialize)]
struct PushCommit {
    #[allow(dead_code)]
    id: String,
    message: String,
    #[allow(dead_code)]
    author: CommitAuthor,
}

#[derive(Debug, Deserialize)]
struct CommitAuthor {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    email: String,
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
    git_sender: mpsc::Sender<GitTask>,
    gitlab_sender: mpsc::Sender<GitLabTask>,
    db_service: DbService,
    channels: Arc<HashMap<String, ChannelConfig>>,
) {
    debug!("Received GitLab webhook event: {}", event_type);

    match event_type {
        "Merge Request Hook" => {
            if let Err(e) = handle_merge_request_event(payload, gitlab_sender, db_service).await {
                warn!("Failed to handle merge request event: {:?}", e);
            }
        },
        "Push Hook" => {
            if let Err(e) = handle_push_event(payload, git_sender, channels).await {
                warn!("Failed to handle push event: {:?}", e);
            }
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

async fn handle_push_event(
    payload: serde_json::Value,
    git_sender: mpsc::Sender<GitTask>,
    channels: Arc<HashMap<String, ChannelConfig>>,
) -> anyhow::Result<()> {
    let event: PushPayload = serde_json::from_value(payload)?;

    // A branch-deletion push delivers `after = "0000…"`; nothing to eval.
    let after_is_zero = event.after.bytes().all(|b| b == b'0');
    if after_is_zero {
        debug!(event = "gitlab_push_branch_deleted", "ignoring branch deletion");
        return Ok(());
    }

    // Extract branch name from ref (e.g., "refs/heads/main" -> "main")
    let Some(branch) = event.ref_name.strip_prefix("refs/heads/") else {
        debug!(
            event = "gitlab_push_non_branch_ref",
            ref_field = event.ref_name,
            "ignoring non-branch push"
        );
        return Ok(());
    };

    let domain = extract_domain(&event.project.web_url);
    let (owner, repo_name) = parse_path_with_namespace(&event.project.path_with_namespace);

    info!(
        "Push to {} on branch '{}' in {}/{} (project {}): {} -> {}",
        domain,
        branch,
        owner,
        repo_name,
        event.project.id,
        &event.before[..8.min(event.before.len())],
        &event.after[..8.min(event.after.len())]
    );

    if !event.commits.is_empty() {
        info!(
            "Push includes {} commit(s), latest: {}",
            event.commits.len(),
            event
                .commits
                .last()
                .map(|c| c.message.lines().next().unwrap_or(""))
                .unwrap_or("")
        );
    }

    // Release-channel routing: a push to a tracking-branch that any
    // channel watches must trigger an evaluation of `.eka-ci/config.json`
    // at the new SHA. The ChannelService (PR 3) consumes the eventual
    // JobSetComplete and decides whether to promote target-branch.
    let forge = ChannelForge::GitLab {
        domain: domain.clone(),
    };
    let matches = match_push_channels(&channels, &forge, &owner, &repo_name, branch);
    if matches.is_empty() {
        debug!(
            event = "gitlab_push_no_channel_match",
            owner = %owner,
            repo = %repo_name,
            branch = %branch,
            "no release channel matches push"
        );
        return Ok(());
    }

    info!(
        event = "gitlab_push_channel_match",
        owner = %owner,
        repo = %repo_name,
        branch = %branch,
        sha = %event.after,
        channels = matches.len(),
        "push to tracking-branch matched release channel(s); scheduling eval"
    );

    let repo = GitRepo {
        protocol: GitProtocol::Https,
        domain,
        owner: owner.clone(),
        repo: repo_name.clone(),
    };
    let workspace = GitWorkspace::from_git_repo(repo, &event.after);
    if let Err(e) = git_sender.send(GitTask::Checkout(workspace)).await {
        warn!(
            event = "gitlab_push_checkout_send_failed",
            error = %e,
            "failed to enqueue GitTask::Checkout"
        );
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
