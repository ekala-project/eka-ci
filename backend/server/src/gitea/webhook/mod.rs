pub mod comment_command;

use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::db::DbService;
use crate::git::GitTask;
use crate::gitea::GiteaTask;

#[derive(Debug, Deserialize)]
struct PullRequestPayload {
    action: String,
    number: i64,
    pull_request: PullRequestData,
    repository: Repository,
    #[allow(dead_code)]
    sender: User,
}

#[derive(Debug, Deserialize)]
struct PullRequestData {
    #[allow(dead_code)]
    number: i64,
    title: String,
    state: String,
    head: BranchRef,
    base: BranchRef,
    user: User,
}

#[derive(Debug, Deserialize)]
struct BranchRef {
    sha: String,
    #[serde(rename = "ref")]
    #[allow(dead_code)]
    ref_name: String,
}

#[derive(Debug, Deserialize)]
struct IssueCommentPayload {
    action: String,
    issue: Issue,
    comment: Comment,
    repository: Repository,
    sender: User,
}

#[derive(Debug, Deserialize)]
struct Issue {
    number: i64,
    pull_request: Option<serde_json::Value>, // Non-null if this is a PR
}

#[derive(Debug, Deserialize)]
struct Comment {
    id: i64,
    body: String,
    created_at: String,
}

#[derive(Debug, Deserialize)]
struct Repository {
    owner: User,
    name: String,
    #[serde(rename = "html_url")]
    html_url: String,
}

#[derive(Debug, Deserialize)]
struct User {
    id: i64,
    login: String,
}

#[derive(Debug, Deserialize)]
struct PushPayload {
    #[serde(rename = "ref")]
    ref_name: String, // e.g., "refs/heads/main"
    before: String, // SHA before push
    after: String,  // SHA after push
    repository: Repository,
    pusher: User,
    commits: Vec<PushCommit>,
}

#[derive(Debug, Deserialize)]
struct PushCommit {
    #[allow(dead_code)]
    id: String,
    message: String,
    #[allow(dead_code)]
    author: CommitUser,
}

#[derive(Debug, Deserialize)]
struct CommitUser {
    #[allow(dead_code)]
    name: String,
    #[allow(dead_code)]
    email: String,
}

/// Handle Gitea webhook payload
///
/// Gitea uses a GitHub-compatible webhook API, but with some differences:
/// - Uses X-Gitea-Event header instead of X-GitHub-Event
/// - Similar but not identical payload structures
/// - Self-hosted instances have domain-specific configuration
/// - Newer versions support check runs, older versions use commit statuses
pub async fn handle_webhook_payload(
    event_type: &str,
    payload: serde_json::Value,
    _git_sender: mpsc::Sender<GitTask>,
    gitea_sender: mpsc::Sender<GiteaTask>,
    db_service: DbService,
) {
    debug!("Received Gitea webhook event: {}", event_type);

    match event_type {
        "pull_request" => {
            if let Err(e) = handle_pull_request_event(payload, gitea_sender, db_service).await {
                warn!("Failed to handle pull request event: {:?}", e);
            }
        },
        "push" => {
            if let Err(e) = handle_push_event(payload).await {
                warn!("Failed to handle push event: {:?}", e);
            }
        },
        "issue_comment" => {
            if let Err(e) = handle_issue_comment_event(payload, gitea_sender).await {
                warn!("Failed to handle issue comment event: {:?}", e);
            }
        },
        "pull_request_review" => {
            debug!("Gitea PR review webhook (ignoring for now)");
            // Review approvals could trigger auto-merge in the future
        },
        _ => {
            warn!("Unhandled Gitea webhook event type: {}", event_type);
        },
    }
}

async fn handle_pull_request_event(
    payload: serde_json::Value,
    gitea_sender: mpsc::Sender<GiteaTask>,
    db_service: DbService,
) -> anyhow::Result<()> {
    let event: PullRequestPayload = serde_json::from_value(payload)?;

    let domain = extract_domain(&event.repository.html_url);
    let owner = &event.repository.owner.login;
    let repo_name = &event.repository.name;
    let pr_number = event.number;

    // Store/update PR in database
    crate::db::gitea::upsert_pull_request(
        pr_number,
        owner,
        repo_name,
        &domain,
        &event.pull_request.head.sha,
        &event.pull_request.base.sha,
        &event.pull_request.title,
        &event.pull_request.user.login,
        &event.pull_request.state,
        &db_service.pool,
    )
    .await?;

    // Trigger auto-merge check on certain actions
    match event.action.as_str() {
        "opened" | "synchronize" | "reopened" => {
            info!(
                "Triggering auto-merge check for PR #{} in {}/{}",
                pr_number, owner, repo_name
            );
            gitea_sender
                .send(GiteaTask::CheckAutoMerge {
                    domain: domain.to_string(),
                    owner: owner.to_string(),
                    repo_name: repo_name.to_string(),
                    pr_number,
                })
                .await?;
        },
        "closed" => {
            if event.pull_request.state == "closed" {
                debug!("PR #{} was closed, no action needed", pr_number);
            } else {
                debug!("PR #{} was merged, no action needed", pr_number);
            }
        },
        _ => {
            debug!(
                "PR #{} action '{}' doesn't trigger auto-merge check",
                pr_number, event.action
            );
        },
    }

    Ok(())
}

async fn handle_issue_comment_event(
    payload: serde_json::Value,
    gitea_sender: mpsc::Sender<GiteaTask>,
) -> anyhow::Result<()> {
    let event: IssueCommentPayload = serde_json::from_value(payload)?;

    // Only process PR comments
    if event.issue.pull_request.is_none() {
        debug!("Comment is not on a pull request, ignoring");
        return Ok(());
    }

    // Only process "created" comments
    if event.action != "created" {
        debug!(
            "Comment action '{}' is not 'created', ignoring",
            event.action
        );
        return Ok(());
    }

    // Check if this is a merge command
    if let Some(command) = comment_command::parse_comment_command(&event.comment.body) {
        let domain = extract_domain(&event.repository.html_url);

        info!(
            "Detected merge command {:?} from {} on PR #{} in {}/{}",
            command,
            event.sender.login,
            event.issue.number,
            event.repository.owner.login,
            event.repository.name
        );

        // Parse the created_at timestamp
        let comment_created_at = chrono::DateTime::parse_from_rfc3339(&event.comment.created_at)
            .ok()
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);

        gitea_sender
            .send(GiteaTask::ProcessMergeCommand {
                domain: domain.to_string(),
                owner: event.repository.owner.login,
                repo_name: event.repository.name,
                pr_number: event.issue.number,
                comment_id: event.comment.id,
                requester_id: event.sender.id,
                requester_login: event.sender.login,
                body: event.comment.body,
                comment_created_at,
            })
            .await?;
    }

    Ok(())
}

async fn handle_push_event(payload: serde_json::Value) -> anyhow::Result<()> {
    let event: PushPayload = serde_json::from_value(payload)?;

    // Extract branch name from ref (e.g., "refs/heads/main" -> "main")
    let branch = event
        .ref_name
        .strip_prefix("refs/heads/")
        .unwrap_or(&event.ref_name);

    let domain = extract_domain(&event.repository.html_url);
    let owner = &event.repository.owner.login;
    let repo_name = &event.repository.name;

    info!(
        "Push to {} on branch '{}' in {}/{}: {} -> {} by {}",
        domain,
        branch,
        owner,
        repo_name,
        &event.before[..8],
        &event.after[..8],
        event.pusher.login
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

    // NOTE: Full CI triggering for main branch builds requires:
    // 1. Determining the repository's default branch (might need API call)
    // 2. Evaluating the CI configuration (e.g., .gitea/workflows/ or eka-ci.nix)
    // 3. Creating a CI job set via GiteaTask::CreateJobSet
    // 4. Managing build state and status updates
    //
    // This is currently not implemented as it requires integration with the
    // CI evaluation and job scheduling system. For now, push events are logged
    // but do not trigger builds.

    debug!("Push event logged but not triggering CI (main branch builds not yet implemented)");

    Ok(())
}

/// Extract domain from Gitea repository HTML URL
fn extract_domain(html_url: &str) -> String {
    html_url
        .strip_prefix("https://")
        .or_else(|| html_url.strip_prefix("http://"))
        .and_then(|s| s.split('/').next())
        .unwrap_or("gitea.local")
        .to_string()
}
