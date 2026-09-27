// GitHub push-event handler for release channels.
//
// On a push to a tracking-branch that any release channel is watching,
// drive a `GitTask::Checkout` for the new SHA so the downstream pipeline
// (RepoTask::Read -> evaluator -> build scheduler) will evaluate
// `.eka-ci/config.json` at that commit. The ChannelService then
// observes the resulting JobSetComplete event and decides whether to
// promote the SHA onto the channel's target-branch.
//
// On a push to a target-branch (channel branch), dispatch a
// `SearchIndexTask::GenerateIndexes` so the search index database
// is regenerated. This fires whether the push came from eka-ci's own
// promotion or an external update.

use std::collections::HashMap;
use std::sync::Arc;

use octocrab::models::webhook_events::payload::PushWebhookEventPayload;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::channels::{match_push_channels, match_target_branch_channels};
use crate::config::{ChannelConfig, ChannelForge};
use crate::git::{GitProtocol, GitRepo, GitTask, GitWorkspace};
use crate::search_index::types::SearchIndexTask;

/// Handle a `push` webhook delivered by GitHub.
///
/// `repository_info` is the `(owner, name)` tuple extracted from the
/// top-level `repository.owner.login` / `repository.name` fields in the
/// webhook envelope (octocrab's `PushWebhookEventPayload` does not
/// itself carry the repository identity).
///
/// The handler checks two things:
///   1. Does the pushed branch match a channel's `tracking_branch`? If so, schedule a CI evaluation
///      (clone + eval + build).
///   2. Does the pushed branch match a channel's `target_branch`? If so, trigger search-index
///      regeneration.
pub(super) async fn handle_github_push(
    payload: PushWebhookEventPayload,
    repository_info: Option<(String, String)>,
    git_sender: mpsc::Sender<GitTask>,
    channels: Arc<HashMap<String, ChannelConfig>>,
    search_index_sender: Option<mpsc::Sender<SearchIndexTask>>,
) {
    // Branch-delete pushes (e.g. closing a PR) arrive with `deleted: true`
    // and `after = "0000…"`. There's nothing to evaluate; ignore.
    if payload.deleted {
        debug!(
            event = "github_push_branch_deleted",
            "Ignoring branch-deletion push"
        );
        return;
    }

    let Some((owner, repo_name)) = repository_info else {
        warn!(
            event = "github_push_missing_repository",
            "Push webhook without repository info; cannot route to channels"
        );
        return;
    };

    // Strip refs/heads/ to recover the branch name. Tag pushes have
    // `refs/tags/...` and currently no channel feature reacts to them,
    // so they fall through both filters and are dropped.
    let Some(branch) = payload.r#ref.strip_prefix("refs/heads/") else {
        debug!(
            event = "github_push_non_branch_ref",
            ref_field = payload.r#ref,
            "Ignoring non-branch push"
        );
        return;
    };

    let after_sha = payload.after;

    // --- Tracking-branch match: schedule CI evaluation ---
    handle_tracking_branch(
        &channels,
        &owner,
        &repo_name,
        branch,
        &after_sha,
        &git_sender,
    )
    .await;

    // --- Target-branch match: trigger search-index generation ---
    handle_target_branch(
        &channels,
        &owner,
        &repo_name,
        branch,
        &after_sha,
        &search_index_sender,
    )
    .await;
}

/// If the pushed branch matches a channel's `tracking_branch`, schedule
/// a git checkout to kick off the eval → build → promotion pipeline.
async fn handle_tracking_branch(
    channels: &HashMap<String, ChannelConfig>,
    owner: &str,
    repo_name: &str,
    branch: &str,
    sha: &str,
    git_sender: &mpsc::Sender<GitTask>,
) {
    let matches = match_push_channels(channels, &ChannelForge::GitHub, owner, repo_name, branch);
    if matches.is_empty() {
        debug!(
            event = "github_push_no_tracking_match",
            owner = %owner,
            repo = %repo_name,
            branch = %branch,
            "Push does not match any tracking branch"
        );
        return;
    }

    info!(
        event = "github_push_channel_match",
        owner = %owner,
        repo = %repo_name,
        branch = %branch,
        sha = %sha,
        channels = matches.len(),
        "Push to tracking-branch matched release channel(s); scheduling eval"
    );

    // De-duplicate Checkout emission: multiple channels can share the
    // same `(owner, repo, branch)`, but they evaluate the SAME commit.
    // One Checkout suffices; the per-channel decision is made later by
    // the ChannelService once the build completes.
    let repo = GitRepo {
        protocol: GitProtocol::Https,
        domain: "github.com".to_string(),
        owner: owner.to_string(),
        repo: repo_name.to_string(),
    };
    let workspace = GitWorkspace::from_git_repo(repo, sha);

    if let Err(e) = git_sender.send(GitTask::Checkout(workspace)).await {
        warn!(
            event = "github_push_checkout_send_failed",
            error = %e,
            owner = %owner,
            repo = %repo_name,
            branch = %branch,
            sha = %sha,
            "Failed to enqueue GitTask::Checkout for channel push"
        );
    }
}

/// If the pushed branch matches a channel's `target_branch`, dispatch
/// search-index generation for each matching channel.
async fn handle_target_branch(
    channels: &HashMap<String, ChannelConfig>,
    owner: &str,
    repo_name: &str,
    branch: &str,
    sha: &str,
    search_index_sender: &Option<mpsc::Sender<SearchIndexTask>>,
) {
    let Some(sender) = search_index_sender else {
        return;
    };

    let matches =
        match_target_branch_channels(channels, &ChannelForge::GitHub, owner, repo_name, branch);
    if matches.is_empty() {
        return;
    }

    for channel in matches {
        info!(
            event = "github_push_target_branch_match",
            owner = %owner,
            repo = %repo_name,
            branch = %branch,
            sha = %sha,
            channel = %channel.name,
            "Push to channel target-branch; triggering search index generation"
        );

        let task = SearchIndexTask::GenerateIndexes {
            channel_id: channel.channel_id(),
            channel: channel.clone(),
            sha: sha.to_string(),
        };

        if let Err(e) = sender.send(task).await {
            warn!(
                event = "search_index_dispatch_failed",
                error = %e,
                channel = %channel.name,
                sha = %sha,
                "Failed to dispatch search index task for target-branch push"
            );
        }
    }
}
