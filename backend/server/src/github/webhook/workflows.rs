// Workflow run webhook handling

use anyhow::{Context, bail};
use octocrab::Octocrab;
use octocrab::models::webhook_events::payload;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::WorkflowRunData;
use crate::git::GitTask;

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
        let git_task = GitTask::GitHubCheckout(Box::new(pull_request));
        git_sender.send(git_task).await?;
    }

    Ok(())
}

pub(super) async fn handle_github_workflow_run(
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
