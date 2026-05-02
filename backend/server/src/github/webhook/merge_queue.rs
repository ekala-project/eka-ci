// Merge queue webhook handling

use std::collections::HashMap;
use std::sync::Arc;

use octocrab::models::webhook_events::payload;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::MergeGroupData;
use crate::config::GitHubAppConfig;
use crate::db::DbService;
use crate::git::GitTask;
use crate::github::GitHubTask;
use crate::github_permissions::{PermissionContext, check_github_app_permission};

pub(super) async fn handle_github_merge_group(
    merge_group_payload: payload::MergeGroupWebhookEventPayload,
    repository_info: Option<(String, String)>,
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    merge_queue_require_approval: bool,
    db_service: DbService,
    github_app_configs: Arc<HashMap<String, GitHubAppConfig>>,
) {
    use payload::MergeGroupWebhookEventAction as MGWEA;

    // This handler should only exist if a valid github_sender also exists
    let _github_sender = match github_sender {
        Some(sender) => sender,
        _ => {
            warn!("GitHub service is down, unable to service merge_group webhook. Restart Eka-CI");
            return;
        },
    };

    // Only handle "checks_requested" action
    if merge_group_payload.action != MGWEA::ChecksRequested {
        debug!(
            "Ignoring merge_group action: {:?}",
            merge_group_payload.action
        );
        return;
    }

    debug!("Received merge_group checks_requested event");

    // Extract repository info
    let (owner, repo_name) = match repository_info {
        Some(info) => info,
        None => {
            warn!("Missing repository info in merge_group webhook");
            return;
        },
    };

    // Parse the merge_group data
    let merge_group: MergeGroupData = match serde_json::from_value(merge_group_payload.merge_group)
    {
        Ok(data) => data,
        Err(e) => {
            warn!("Failed to parse merge_group data: {:?}", e);
            return;
        },
    };

    debug!(
        "Processing merge queue commit {} for ref {} in {}/{}",
        merge_group.head_sha, merge_group.head_ref, owner, repo_name
    );

    // Check GitHub App permissions before processing merge group
    let permission_context = PermissionContext {
        repo_owner: owner.clone(),
        repo_name: repo_name.clone(),
        branch: Some(merge_group.base_ref.clone()),
    };

    // Check if any configured GitHub App has permission for this repo/branch
    let has_permission = github_app_configs
        .values()
        .any(|config| check_github_app_permission(config, &permission_context).is_ok());

    if !has_permission {
        warn!(
            "No GitHub App has permission for repository {}/{} branch {}. Skipping merge group \
             for commit {}",
            owner, repo_name, merge_group.base_ref, merge_group.head_sha
        );
        return;
    }

    // Check if approval is required for merge queue builds
    if merge_queue_require_approval {
        let author = &merge_group.head_commit.author.name;

        // Check if the author is approved
        // Note: Merge groups don't provide user_id, so we use 0 as a sentinel
        // The approval check will only check by username in this case
        match db_service.is_user_approved(author, 0).await {
            Ok(true) => {
                debug!("Merge queue build approved for user: {}", author);
            },
            Ok(false) => {
                warn!(
                    "Merge queue build requires approval. User {} is not approved for {}/{}",
                    author, owner, repo_name
                );
                return;
            },
            Err(e) => {
                warn!(
                    "Failed to check approval status for user {}: {:?}",
                    author, e
                );
                return;
            },
        }
    }

    // Store merge queue build in database
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = db_service
        .upsert_merge_queue_build(
            &owner,
            &repo_name,
            &merge_group.head_sha,
            &merge_group.base_sha,
            &merge_group.base_ref,
            &merge_group.head_sha, // merge_group_head_sha is same as head_sha
            &now,
            &now,
        )
        .await
    {
        warn!(
            "Failed to store merge queue build for commit {}: {:?}",
            merge_group.head_sha, e
        );
        // Continue processing even if DB storage fails
    }

    // Create a synthetic PullRequest to reuse the existing GitTask::GitHubCheckout flow
    // This allows us to leverage all the existing git checkout and CI evaluation logic
    use octocrab::models::Repository as OctoRepo;
    use octocrab::models::pulls::{Head, PullRequest};

    // Create minimal repository object
    let repo_value = serde_json::json!({
        "name": repo_name,
        "owner": {
            "login": owner
        },
        "clone_url": format!("https://github.com/{}/{}.git", owner, repo_name),
    });
    let repo: OctoRepo = match serde_json::from_value(repo_value) {
        Ok(r) => r,
        Err(e) => {
            warn!("Failed to create repository object: {:?}", e);
            return;
        },
    };

    // Create head and base objects for the merge queue commit
    let head_value = serde_json::json!({
        "ref": merge_group.head_ref,
        "sha": merge_group.head_sha,
        "repo": repo.clone(),
    });
    let head: Head = match serde_json::from_value(head_value) {
        Ok(h) => h,
        Err(e) => {
            warn!("Failed to create head object: {:?}", e);
            return;
        },
    };

    let base_value = serde_json::json!({
        "ref": merge_group.base_ref,
        "sha": merge_group.base_sha,
        "repo": repo.clone(),
    });
    let base: Head = match serde_json::from_value(base_value) {
        Ok(b) => b,
        Err(e) => {
            warn!("Failed to create base object: {:?}", e);
            return;
        },
    };

    // Create a synthetic PullRequest object
    let pr_value = serde_json::json!({
        "number": 0, // Merge queue doesn't have a PR number in this context
        "state": "open",
        "title": format!("Merge queue for {}", merge_group.base_ref),
        "head": head,
        "base": base,
        "user": {
            "login": merge_group.head_commit.author.name,
            "id": 0,
        },
    });

    let pr: PullRequest = match serde_json::from_value(pr_value) {
        Ok(p) => p,
        Err(e) => {
            warn!("Failed to create synthetic PullRequest: {:?}", e);
            return;
        },
    };

    // Send the checkout task using the existing flow
    let git_task = GitTask::GitHubCheckout(pr);

    if let Err(e) = git_sender.send(git_task).await {
        warn!("Failed to send merge queue checkout task: {:?}", e);
    } else {
        debug!(
            "Successfully queued merge queue checkout task for commit {}",
            merge_group.head_sha
        );
    }
}
