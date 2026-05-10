// GitHub webhook event handling

use std::collections::HashMap;
use std::sync::Arc;

use octocrab::Octocrab;
use octocrab::models::webhook_events::{EventInstallation, WebhookEventPayload as WEP};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::config::{ChannelConfig, GitHubAppConfig};
use crate::db::DbService;
use crate::git::GitTask;
use crate::github::GitHubTask;

pub(crate) mod comment_command;
mod comments;
mod installations;
mod merge_queue;
mod pull_requests;
mod push;
mod reviews;
mod workflows;

// Webhook payload data structures

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

// Main webhook dispatcher

#[allow(clippy::too_many_arguments)]
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
    channels: Arc<HashMap<String, ChannelConfig>>,
) {
    match webhook_payload {
        WEP::PullRequest(pr) => {
            pull_requests::handle_github_pr(
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
            workflows::handle_github_workflow_run(*workflow_run, git_sender, octocrab).await
        },
        WEP::MergeGroup(merge_group) => {
            merge_queue::handle_github_merge_group(
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
            installations::handle_github_installation(
                *installation_payload,
                installation,
                db_service,
            )
            .await
        },
        WEP::InstallationRepositories(installation_repos) => {
            installations::handle_github_installation_repositories(
                *installation_repos,
                installation,
                db_service,
            )
            .await
        },
        WEP::PullRequestReview(review) => {
            reviews::handle_github_pr_review(*review, github_sender, db_service).await
        },
        WEP::IssueComment(comment_event) => {
            comments::handle_github_issue_comment(*comment_event, repository_info, github_sender)
                .await
        },
        WEP::Push(push_payload) => {
            push::handle_github_push(*push_payload, repository_info, git_sender, channels).await
        },
        _ => (),
    }
}
