use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, bail};
use octocrab::Octocrab;
use octocrab::models::pulls::PullRequest;
use octocrab::models::webhook_events::{EventInstallation, WebhookEventPayload as WEP, payload};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::config::GitHubAppConfig;
use crate::db::DbService;
use crate::git::GitTask;
use crate::github::{CICheckInfo, GitHubTask};
use crate::github_permissions::{PermissionContext, check_github_app_permission};

pub async fn handle_webhook_payload(
    webhook_payload: WEP,
    repository_info: Option<(String, String)>, // (owner, repo_name)
    installation: Option<EventInstallation>,
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    octocrab: Option<Octocrab>,
    require_approval: bool,
    merge_queue_require_approval: bool,
    db_service: DbService,
    github_app_configs: Arc<HashMap<String, GitHubAppConfig>>,
) {
    match webhook_payload {
        WEP::PullRequest(pr) => {
            handle_github_pr(
                *pr,
                git_sender,
                github_sender,
                require_approval,
                db_service,
                github_app_configs,
            )
            .await
        },
        WEP::WorkflowRun(workflow_run) => {
            handle_github_workflow_run(*workflow_run, git_sender, octocrab).await
        },
        WEP::MergeGroup(merge_group) => {
            handle_github_merge_group(
                *merge_group,
                repository_info,
                git_sender,
                github_sender,
                merge_queue_require_approval,
                db_service.clone(),
                github_app_configs,
            )
            .await
        },
        WEP::Installation(installation_payload) => {
            handle_github_installation(*installation_payload, installation, db_service).await
        },
        WEP::InstallationRepositories(installation_repos) => {
            handle_github_installation_repositories(*installation_repos, installation, db_service)
                .await
        },
        WEP::PullRequestReview(review) => {
            handle_github_pr_review(*review, github_sender, db_service).await
        },
        // We probably don't want to react to every push
        // WEP::Push(pr) => handle_github_push(*pr).await,
        _ => (),
    }
}

#[derive(Debug, Deserialize)]
struct WorkflowRunPullRequest {
    number: u64,
}

#[derive(Debug, Deserialize)]
struct WorkflowRunData {
    pull_requests: Vec<WorkflowRunPullRequest>,
    repository: WorkflowRunRepository,
}

#[derive(Debug, Deserialize)]
struct WorkflowRunRepository {
    name: String,
    owner: WorkflowRunOwner,
}

#[derive(Debug, Deserialize)]
struct WorkflowRunOwner {
    login: String,
}

#[derive(Debug, Deserialize)]
struct MergeGroupData {
    head_sha: String,
    head_ref: String,
    base_sha: String,
    base_ref: String,
    head_commit: MergeGroupCommit,
}

// Webhook payload structs — fields must match GitHub's JSON schema for
// deserialization to succeed, even when Rust code doesn't currently read them.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct MergeGroupCommit {
    id: String,
    message: String,
    author: MergeGroupUser,
    committer: MergeGroupUser,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct MergeGroupUser {
    name: String,
    email: String,
}

async fn is_valid_user(pr: &PullRequest, db_service: &DbService) -> anyhow::Result<()> {
    let pr_author = &pr.user.as_ref().context("Missing github user for PR")?;
    let username = &pr_author.login;
    let user_id = pr_author.id.0 as i64;

    if !db_service.is_user_approved(username, user_id).await? {
        bail!("User is not approved for workflows");
    }

    Ok(())
}

/// Store PR metadata in the database
async fn store_pr_metadata(pr: &PullRequest, state: &str, db_service: &DbService) {
    let pr_author = match &pr.user {
        Some(user) => &user.login,
        None => {
            warn!(
                "PR #{} has no user information, skipping metadata storage",
                pr.number
            );
            return;
        },
    };

    let pr_number = pr.number as i64;

    let owner = match &pr.base.repo {
        Some(repo) => match &repo.owner {
            Some(owner) => &owner.login,
            None => {
                warn!("PR base repo has no owner");
                return;
            },
        },
        None => {
            warn!("PR base has no repo");
            return;
        },
    };

    let repo_name = match &pr.base.repo {
        Some(repo) => &repo.name,
        None => {
            warn!("PR base has no repo");
            return;
        },
    };

    let head_sha = &pr.head.sha;
    let base_sha = &pr.base.sha;

    let title = match &pr.title {
        Some(t) => t.as_str(),
        None => "",
    };

    let created_at = pr.created_at.map(|dt| dt.to_rfc3339()).unwrap_or_default();
    let updated_at = pr.updated_at.map(|dt| dt.to_rfc3339()).unwrap_or_default();

    if let Err(e) = db_service
        .upsert_pull_request(
            pr_number,
            owner,
            repo_name,
            head_sha,
            base_sha,
            title,
            pr_author,
            state,
            &created_at,
            &updated_at,
        )
        .await
    {
        warn!("Failed to store PR metadata for PR #{}: {:?}", pr_number, e);
    } else {
        debug!("Stored metadata for PR #{}", pr_number);
    }
}

async fn handle_github_pr(
    pr: payload::PullRequestWebhookEventPayload,
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    require_approval: bool,
    db_service: DbService,
    github_app_configs: Arc<HashMap<String, GitHubAppConfig>>,
) {
    use payload::PullRequestWebhookEventAction as PRWEA;

    // This handler should only exist if a valid github_sender also exsits,
    // should be safe to assume this will always succeed
    let github_sender = match github_sender {
        Some(sender) => sender,
        _ => {
            warn!("GitHub service is down, unable to service webhook request. Restart Eka-CI");
            return;
        },
    };

    match pr.action {
        PRWEA::Opened | PRWEA::Synchronize | PRWEA::Reopened => {
            debug!("Received event for PR #{}", &pr.pull_request.number);

            // Check GitHub App permissions before processing
            if let Some(base_repo) = &pr.pull_request.base.repo {
                let owner = &base_repo
                    .owner
                    .as_ref()
                    .map(|o| o.login.clone())
                    .unwrap_or_default();
                let repo_name = &base_repo.name;
                let branch = &pr.pull_request.base.ref_field;

                let permission_context = PermissionContext {
                    repo_owner: owner.clone(),
                    repo_name: repo_name.clone(),
                    branch: Some(branch.clone()),
                };

                // Check if any configured GitHub App has permission for this repo/branch
                let has_permission = github_app_configs
                    .values()
                    .any(|config| check_github_app_permission(config, &permission_context).is_ok());

                if !has_permission {
                    warn!(
                        "No GitHub App has permission for repository {}/{} branch {}. Skipping PR \
                         #{}",
                        owner, repo_name, branch, pr.pull_request.number
                    );
                    return;
                }
            }

            // Store PR metadata in database
            store_pr_metadata(&pr.pull_request, "open", &db_service).await;

            if require_approval && is_valid_user(&pr.pull_request, &db_service).await.is_err() {
                if let Err(e) = github_sender
                    .send(GitHubTask::CreateApprovalRequiredCheckRun {
                        ci_check_info: CICheckInfo::from_gh_pr_head(&pr.pull_request),
                        username: pr.pull_request.user.unwrap().login.clone(),
                    })
                    .await
                {
                    warn!("Failed to send approval required check run task: {:?}", e);
                }

                // Don't proceed until workflow is approved
                return;
            }

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

            // Update PR state in database
            let state = if matches!(pr.action, PRWEA::Closed) {
                // Determine if merged or just closed
                if pr.pull_request.merged_at.is_some() {
                    "merged"
                } else {
                    "closed"
                }
            } else {
                "open" // ConvertedToDraft keeps it open
            };
            store_pr_metadata(&pr.pull_request, state, &db_service).await;

            let ci_check_info = CICheckInfo::from_gh_pr_head(&pr.pull_request);

            if let Err(e) = github_sender
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
        },
        PRWEA::Enqueued => {
            debug!(
                "Received enqueued event for PR #{}",
                &pr.pull_request.number
            );

            // Store PR metadata in database
            store_pr_metadata(&pr.pull_request, "open", &db_service).await;

            if require_approval && is_valid_user(&pr.pull_request, &db_service).await.is_err() {
                if let Err(e) = github_sender
                    .send(GitHubTask::CreateApprovalRequiredCheckRun {
                        ci_check_info: CICheckInfo::from_gh_pr_head(&pr.pull_request),
                        username: pr.pull_request.user.unwrap().login.clone(),
                    })
                    .await
                {
                    warn!("Failed to send approval required check run task: {:?}", e);
                }

                // Don't proceed until workflow is approved
                return;
            }

            let git_task = GitTask::GitHubCheckout(pr.pull_request.clone());

            if let Err(e) = git_sender.send(git_task).await {
                warn!("Failed to send PR checkout task for enqueued PR: {:?}", e);
            } else {
                debug!(
                    "Successfully queued PR checkout task for enqueued PR #{}",
                    pr.pull_request.number
                );
            }
        },
        action => {
            debug!("Ignoring non-actionable PR action: {:?}", &action);
        },
    }
}

async fn handle_github_workflow_requested(
    payload: WorkflowRunData,
    git_sender: mpsc::Sender<GitTask>,
    octocrab: Option<Octocrab>,
) -> anyhow::Result<()> {
    // Check if there are any pull requests associated with this workflow run
    if payload.pull_requests.is_empty() {
        bail!("No pull requests associated with this workflow_run (likely from a fork or push)");
    }

    let owner = payload.repository.owner.login;
    let repo_name = payload.repository.name;

    // Fetch the full PR details using octocrab
    let octocrab = octocrab.context("GitHub App octocrab instance not available")?;

    for pr_number in payload.pull_requests {
        debug!(
            "Workflow approved for PR #{} in {}/{}",
            &pr_number.number, &owner, &repo_name
        );
        let pull_request = octocrab
            .pulls(&owner, &repo_name)
            .get(pr_number.number)
            .await?;
        // Workflow was approved by maintainer, send the PR for checkout
        let git_task = GitTask::GitHubCheckout(pull_request);
        git_sender.send(git_task).await?;
    }

    Ok(())
}

async fn handle_github_workflow_run(
    workflow_run: payload::WorkflowRunWebhookEventPayload,
    git_sender: mpsc::Sender<GitTask>,
    octocrab: Option<Octocrab>,
) {
    use payload::WorkflowRunWebhookEventAction as WRWEA;

    // Only handle "requested" action (when workflow is approved by maintainer)
    if workflow_run.action != WRWEA::Requested {
        debug!("Ignoring workflow_run action: {:?}", workflow_run.action);
        return;
    }

    debug!("Received workflow_run requested event");

    // Parse the workflow_run data to extract PR information
    let workflow_run_data: WorkflowRunData = match serde_json::from_value(workflow_run.workflow_run)
    {
        Ok(data) => data,
        Err(e) => {
            warn!("Failed to parse workflow_run data: {:?}", e);
            return;
        },
    };

    if let Err(e) = handle_github_workflow_requested(workflow_run_data, git_sender, octocrab).await
    {
        warn!("Failed to process github workflow requested: {}", e);
    }
}

async fn handle_github_merge_group(
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

/// Handle `pull_request_review` webhook events.
///
/// When a reviewer submits an approval (or dismisses/changes an existing review),
/// re-check auto-merge eligibility for the PR. This ensures approvals that arrive
/// after builds have completed still trigger the merge flow, rather than waiting
/// for a subsequent build to reach the recorder's end-of-jobset hook.
async fn handle_github_pr_review(
    review_event: payload::PullRequestReviewWebhookEventPayload,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    db_service: DbService,
) {
    use octocrab::models::pulls::ReviewState;
    use payload::PullRequestReviewWebhookEventAction as PRRWEA;

    let Some(github_sender) = github_sender else {
        warn!("GitHub service is down, unable to service review webhook. Restart Eka-CI");
        return;
    };

    // Only react to events that actually change approval state. Edits to a
    // review body do not change whether the PR is approved.
    match review_event.action {
        PRRWEA::Submitted => {
            // Only approving reviews affect merge eligibility in a positive
            // direction. Non-approving reviews (Commented, ChangesRequested)
            // would only ever prevent a merge, which the existing approval
            // check already handles at merge time, so we skip re-triggering.
            if review_event.review.state != Some(ReviewState::Approved) {
                debug!(
                    "Ignoring non-approving review on PR #{}",
                    review_event.pull_request.number
                );
                return;
            }
        },
        PRRWEA::Dismissed => {
            // A dismissal can affect later recomputation; re-run the check so
            // any cached state is refreshed.
        },
        action => {
            debug!("Ignoring pull_request_review action: {:?}", action);
            return;
        },
    }

    let pr = &review_event.pull_request;
    let pr_number = pr.number as i64;

    let Some(base_repo) = pr.base.repo.as_ref() else {
        warn!(
            "pull_request_review for PR #{} missing base repo, cannot check auto-merge",
            pr_number
        );
        return;
    };

    let Some(owner) = base_repo.owner.as_ref().map(|o| o.login.clone()) else {
        warn!(
            "pull_request_review for PR #{} missing base repo owner, cannot check auto-merge",
            pr_number
        );
        return;
    };
    let repo_name = base_repo.name.clone();

    // Only consider PRs where auto-merge has been enabled and which are still
    // open. This matches the gate applied by the scheduler-driven path.
    let pr_record = match crate::db::github::get_pr_by_head_sha(
        &pr.head.sha,
        &owner,
        &repo_name,
        &db_service.pool,
    )
    .await
    {
        Ok(Some(record)) => record,
        Ok(None) => {
            debug!(
                "No stored PR record for {}/{}#{} (head {}); skipping review-triggered auto-merge",
                owner, repo_name, pr_number, pr.head.sha
            );
            return;
        },
        Err(e) => {
            warn!(
                "Failed to look up PR record for {}/{}#{}: {:?}",
                owner, repo_name, pr_number, e
            );
            return;
        },
    };

    if !pr_record.auto_merge_enabled || pr_record.state != "open" {
        debug!(
            "PR #{} auto-merge not enabled or not open (enabled={}, state={}); skipping",
            pr_number, pr_record.auto_merge_enabled, pr_record.state
        );
        return;
    }

    let task = GitHubTask::CheckAutoMerge {
        owner: owner.clone(),
        repo_name: repo_name.clone(),
        pr_number,
        head_sha: pr.head.sha.clone(),
    };

    if let Err(e) = github_sender.send(task).await {
        warn!(
            "Failed to send CheckAutoMerge for PR #{} after review: {:?}",
            pr_number, e
        );
    } else {
        debug!(
            "Queued CheckAutoMerge for PR #{} in {}/{} after review event",
            pr_number, owner, repo_name
        );
    }
}

async fn handle_github_installation(
    installation_payload: payload::InstallationWebhookEventPayload,
    installation: Option<EventInstallation>,
    db_service: DbService,
) {
    use payload::InstallationWebhookEventAction as IWEA;

    // Extract the full installation object from the event
    let installation = match installation {
        Some(EventInstallation::Full(inst)) => inst,
        _ => {
            warn!("Installation event missing full installation object");
            return;
        },
    };

    let installation_id = installation.id.0 as i64;
    let account_type = &installation.account.r#type;
    let account_login = &installation.account.login;

    debug!(
        "Received installation event: {:?} for installation {} ({})",
        installation_payload.action, installation_id, account_login
    );

    match installation_payload.action {
        IWEA::Created => {
            // App was installed
            if let Err(e) = db_service
                .upsert_installation(installation_id, account_type, account_login)
                .await
            {
                warn!(
                    "Failed to upsert installation {} for {}: {}",
                    installation_id, account_login, e
                );
                return;
            }

            // Add all repositories that came with the installation
            if let Some(repos) = installation_payload.repositories {
                for repo in repos {
                    let repo_id = repo.id.0 as i64;
                    let repo_name = &repo.name;
                    // Extract owner from full_name (format: "owner/repo")
                    let repo_owner = repo.full_name.split('/').next().unwrap_or(account_login);

                    if let Err(e) = db_service
                        .upsert_installation_repository(
                            installation_id,
                            repo_id,
                            repo_name,
                            repo_owner,
                        )
                        .await
                    {
                        warn!(
                            "Failed to add repository {} to installation {}: {}",
                            repo_name, installation_id, e
                        );
                    } else {
                        debug!(
                            "Added repository {}/{} to installation {}",
                            repo_owner, repo_name, installation_id
                        );
                    }
                }
            }
        },
        IWEA::Deleted => {
            // App was uninstalled
            if let Err(e) = db_service.delete_installation(installation_id).await {
                warn!("Failed to delete installation {}: {}", installation_id, e);
            } else {
                debug!("Deleted installation {}", installation_id);
            }
        },
        IWEA::Suspend => {
            // App was suspended
            if let Err(e) = db_service.suspend_installation(installation_id).await {
                warn!("Failed to suspend installation {}: {}", installation_id, e);
            } else {
                debug!("Suspended installation {}", installation_id);
            }
        },
        IWEA::Unsuspend => {
            // App was unsuspended
            if let Err(e) = db_service.unsuspend_installation(installation_id).await {
                warn!(
                    "Failed to unsuspend installation {}: {}",
                    installation_id, e
                );
            } else {
                debug!("Unsuspended installation {}", installation_id);
            }
        },
        action => {
            debug!(
                "Ignoring installation action {:?} for installation {}",
                action, installation_id
            );
        },
    }
}

async fn handle_github_installation_repositories(
    installation_repos_payload: payload::InstallationRepositoriesWebhookEventPayload,
    installation: Option<EventInstallation>,
    db_service: DbService,
) {
    use payload::InstallationRepositoriesWebhookEventAction as IRWEA;

    // Extract the installation object from the event
    let installation = match installation {
        Some(EventInstallation::Full(inst)) => inst,
        Some(EventInstallation::Minimal(inst)) => {
            // For installation_repositories events, we might get minimal installation
            // We can still work with just the ID
            debug!("Got minimal installation: {:?}", inst);
            // We'll need the installation ID
            let installation_id = inst.id.0 as i64;

            debug!(
                "Received installation_repositories event: {:?} for installation {}",
                installation_repos_payload.action, installation_id
            );

            match installation_repos_payload.action {
                IRWEA::Added => {
                    // Repositories were added to the installation
                    for repo in installation_repos_payload.repositories_added {
                        let repo_id = repo.id.0 as i64;
                        let repo_name = &repo.name;
                        // Extract owner from full_name (format: "owner/repo")
                        let repo_owner = repo.full_name.split('/').next().unwrap_or("");

                        if let Err(e) = db_service
                            .upsert_installation_repository(
                                installation_id,
                                repo_id,
                                repo_name,
                                repo_owner,
                            )
                            .await
                        {
                            warn!(
                                "Failed to add repository {} to installation {}: {}",
                                repo_name, installation_id, e
                            );
                        } else {
                            debug!(
                                "Added repository {}/{} to installation {}",
                                repo_owner, repo_name, installation_id
                            );
                        }
                    }
                },
                IRWEA::Removed => {
                    // Repositories were removed from the installation
                    for repo in installation_repos_payload.repositories_removed {
                        let repo_id = repo.id.0 as i64;

                        if let Err(e) = db_service
                            .delete_installation_repository(installation_id, repo_id)
                            .await
                        {
                            warn!(
                                "Failed to remove repository {} from installation {}: {}",
                                repo_id, installation_id, e
                            );
                        } else {
                            debug!(
                                "Removed repository {} from installation {}",
                                repo_id, installation_id
                            );
                        }
                    }
                },
                action => {
                    debug!(
                        "Ignoring installation_repositories action {:?} for installation {}",
                        action, installation_id
                    );
                },
            }
            return;
        },
        None => {
            warn!("Installation_repositories event missing installation object");
            return;
        },
    };

    let installation_id = installation.id.0 as i64;

    debug!(
        "Received installation_repositories event: {:?} for installation {}",
        installation_repos_payload.action, installation_id
    );

    match installation_repos_payload.action {
        IRWEA::Added => {
            // Repositories were added to the installation
            for repo in installation_repos_payload.repositories_added {
                let repo_id = repo.id.0 as i64;
                let repo_name = &repo.name;
                // Extract owner from full_name (format: "owner/repo")
                let repo_owner = repo
                    .full_name
                    .split('/')
                    .next()
                    .unwrap_or(&installation.account.login);

                if let Err(e) = db_service
                    .upsert_installation_repository(installation_id, repo_id, repo_name, repo_owner)
                    .await
                {
                    warn!(
                        "Failed to add repository {} to installation {}: {}",
                        repo_name, installation_id, e
                    );
                } else {
                    debug!(
                        "Added repository {}/{} to installation {}",
                        repo_owner, repo_name, installation_id
                    );
                }
            }
        },
        IRWEA::Removed => {
            // Repositories were removed from the installation
            for repo in installation_repos_payload.repositories_removed {
                let repo_id = repo.id.0 as i64;

                if let Err(e) = db_service
                    .delete_installation_repository(installation_id, repo_id)
                    .await
                {
                    warn!(
                        "Failed to remove repository {} from installation {}: {}",
                        repo_id, installation_id, e
                    );
                } else {
                    debug!(
                        "Removed repository {} from installation {}",
                        repo_id, installation_id
                    );
                }
            }
        },
        action => {
            debug!(
                "Ignoring installation_repositories action {:?} for installation {}",
                action, installation_id
            );
        },
    }
}
