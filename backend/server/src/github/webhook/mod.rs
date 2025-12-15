use anyhow::{Context, bail};
use octocrab::models::pulls::PullRequest;
use octocrab::models::webhook_events::{WebhookEventPayload as WEP, payload};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::git::GitTask;
use crate::github::{CICheckInfo, GitHubTask};

pub async fn handle_webhook_payload(
    webhook_payload: WEP,
    git_sender: mpsc::Sender<GitTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    require_approval: bool,
    db_service: DbService,
) {
    match webhook_payload {
        WEP::PullRequest(pr) => {
            handle_github_pr(*pr, git_sender, github_sender, require_approval, db_service).await
        },
        // We probably don't want to react to every push
        // WEP::Push(pr) => handle_github_push(*pr).await,
        _ => return,
    };
    return;
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

#[allow(dead_code)]
// base_ref is optional, so may actually be hard to determine what we can "diff"
// the evaluation from
async fn handle_github_push(push: payload::PushWebhookEventPayload) {
    debug!("Received github push notification: {:?}", push.head_commit);
}

#[allow(dead_code)]
async fn eval_pr(_pr: PullRequest) {}
