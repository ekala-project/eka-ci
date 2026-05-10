// GitHub push-event handler for release channels.
//
// On a push to a tracking-branch that any release channel is watching,
// drive a `GitTask::Checkout` for the new SHA so the downstream pipeline
// (RepoTask::Read -> evaluator -> build scheduler) will evaluate
// `.eka-ci/config.json` at that commit. The ChannelService (PR 3) then
// observes the resulting JobSetComplete event and decides whether to
// promote the SHA onto the channel's target-branch.
//
// PR 2 deliberately stops here: it does NOT touch ChannelPromotion
// rows, does NOT post GitHub Check-Run output, and does NOT push the
// target-branch. Those are wired up in PR 3 and PR 4. The contract for
// this file is "make sure the tracking-branch SHA gets evaluated".

use std::collections::HashMap;
use std::sync::Arc;

use octocrab::models::webhook_events::payload::PushWebhookEventPayload;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::channels::match_push_channels;
use crate::config::{ChannelConfig, ChannelForge};
use crate::git::{GitProtocol, GitRepo, GitTask, GitWorkspace};

/// Handle a `push` webhook delivered by GitHub.
///
/// `repository_info` is the `(owner, name)` tuple extracted from the
/// top-level `repository.owner.login` / `repository.name` fields in the
/// webhook envelope (octocrab's `PushWebhookEventPayload` does not
/// itself carry the repository identity).
///
/// When no configured channel matches the `(owner, repo, branch)` tuple
/// the function short-circuits silently — the vast majority of pushes
/// are for branches eka-ci doesn't gate, and we want to avoid spamming
/// the log.
pub(super) async fn handle_github_push(
    payload: PushWebhookEventPayload,
    repository_info: Option<(String, String)>,
    git_sender: mpsc::Sender<GitTask>,
    channels: Arc<HashMap<String, ChannelConfig>>,
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
    // so they fall through the channel filter and are dropped below.
    let Some(branch) = payload.r#ref.strip_prefix("refs/heads/") else {
        debug!(
            event = "github_push_non_branch_ref",
            ref_field = payload.r#ref,
            "Ignoring non-branch push"
        );
        return;
    };

    let matches =
        match_push_channels(&channels, &ChannelForge::GitHub, &owner, &repo_name, branch);
    if matches.is_empty() {
        // Hot path for the typical case of "push to a branch no channel
        // is watching" — log at debug, not info.
        debug!(
            event = "github_push_no_channel_match",
            owner = %owner,
            repo = %repo_name,
            branch = %branch,
            "Push received but no release channel matches"
        );
        return;
    }

    let after_sha = payload.after;
    info!(
        event = "github_push_channel_match",
        owner = %owner,
        repo = %repo_name,
        branch = %branch,
        sha = %after_sha,
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
        owner: owner.clone(),
        repo: repo_name.clone(),
    };
    let workspace = GitWorkspace::from_git_repo(repo, &after_sha);

    if let Err(e) = git_sender.send(GitTask::Checkout(workspace)).await {
        warn!(
            event = "github_push_checkout_send_failed",
            error = %e,
            owner = %owner,
            repo = %repo_name,
            branch = %branch,
            sha = %after_sha,
            "Failed to enqueue GitTask::Checkout for channel push"
        );
    }
}
