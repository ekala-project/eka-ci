use anyhow::{Context, bail};
use octocrab::Octocrab;
use octocrab::models::Installation;
use octocrab::models::pulls::PullRequest;
use octocrab::models::webhook_events::{WebhookEventPayload as WEP, payload};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::git::GitTask;
use crate::github::{CICheckInfo, GitHubTask};

// Custom struct to properly deserialize installation webhooks from GitHub
// octocrab's InstallationWebhookEventPayload is incomplete and missing the installation field
#[derive(Debug, Clone, Deserialize)]
pub struct InstallationWebhookPayload {
    pub action: String,
    pub installation: Installation,
}

pub async fn handle_webhook_payload(
    webhook_payload: WEP,
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    octocrab: Option<Octocrab>,
    require_approval: bool,
    db_service: DbService,
) {
    match webhook_payload {
        WEP::PullRequest(pr) => {
            handle_github_pr(*pr, git_sender, github_sender, require_approval, db_service).await
        },
        WEP::WorkflowRun(workflow_run) => {
            handle_github_workflow_run(*workflow_run, git_sender, octocrab).await
        },
        // Installation webhooks are handled separately via web.rs
        // because octocrab's types are incomplete
        WEP::Installation(_installation) => {
            // This should not be reached as installation webhooks are
            // intercepted in web.rs before octocrab deserialization
            warn!("Installation webhook reached octocrab handler - this shouldn't happen");
        },
        // Installation webhooks are handled separately in handle_installation_webhook
        // due to incomplete octocrab types
        // We probably don't want to react to every push
        // WEP::Push(pr) => handle_github_push(*pr).await,
        _ => return,
    };
    return;
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

async fn is_valid_user(pr: &PullRequest, db_service: &DbService) -> anyhow::Result<()> {
    let pr_author = &pr.user.as_ref().context("Missing github user for PR")?;
    let username = &pr_author.login;
    let user_id = pr_author.id.0 as i64;

    if !db_service.is_user_approved(username, user_id).await? {
        bail!("User is not approved for workflows");
    }

    Ok(())
}

async fn handle_github_pr(
    pr: payload::PullRequestWebhookEventPayload,
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    require_approval: bool,
    db_service: DbService,
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

            if require_approval && (!is_valid_user(&pr.pull_request, &db_service).await.is_ok()) {
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
            // TODO: Determine what to do for merge queues. If ever relevant
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

pub async fn handle_installation_webhook(
    payload: InstallationWebhookEventPayload,
    db_service: DbService,
) {
    use octocrab::models::webhook_events::payload::InstallationWebhookEventAction as IWEA;
    let installation_id = payload.installation.id.0 as i64;
    let account_login = &payload.installation.account.login;

    match &payload.action {
        IWEA::Created => {
            debug!(
                "GitHub App installed: {} (ID: {})",
                account_login, installation_id
            );

            if let Err(e) = db_service
                .upsert_github_installation(&payload.installation)
                .await
            {
                warn!(
                    "Failed to persist installation {} to database: {:?}",
                    installation_id, e
                );
            } else {
                debug!(
                    "Successfully persisted installation {} to database",
                    installation_id
                );
            }
        },
        IWEA::Deleted => {
            debug!(
                "GitHub App uninstalled: {} (ID: {})",
                account_login, installation_id
            );

            if let Err(e) = db_service.delete_github_installation(installation_id).await {
                warn!(
                    "Failed to delete installation {} from database: {:?}",
                    installation_id, e
                );
            } else {
                debug!(
                    "Successfully deleted installation {} from database",
                    installation_id
                );
            }
        },
        IWEA::Suspend => {
            debug!(
                "GitHub App suspended: {} (ID: {})",
                account_login, installation_id
            );

            if let Err(e) = db_service
                .suspend_github_installation(installation_id)
                .await
            {
                warn!(
                    "Failed to suspend installation {} in database: {:?}",
                    installation_id, e
                );
            } else {
                debug!(
                    "Successfully suspended installation {} in database",
                    installation_id
                );
            }
        },
        IWEA::Unsuspend => {
            debug!(
                "GitHub App unsuspended: {} (ID: {})",
                account_login, installation_id
            );

            if let Err(e) = db_service
                .unsuspend_github_installation(installation_id)
                .await
            {
                warn!(
                    "Failed to unsuspend installation {} in database: {:?}",
                    installation_id, e
                );
            } else {
                debug!(
                    "Successfully unsuspended installation {} in database",
                    installation_id
                );
            }
        },
        action => {
            debug!("Ignoring installation action: {}", action);
        },
    }
}
