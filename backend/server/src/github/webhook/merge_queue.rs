// Merge queue webhook handling

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use octocrab::models::pulls::PullRequest;
use octocrab::models::webhook_events::payload;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::MergeGroupData;
use crate::config::GitHubAppConfig;
use crate::db::DbService;
use crate::git::GitTask;
use crate::github::GitHubTask;
use crate::github_permissions::{PermissionContext, check_github_app_permission};

/// A merge-queue group's commit, as needed to evaluate it.
#[derive(Debug, Clone)]
pub(crate) struct MergeGroup {
    pub owner: String,
    pub repo_name: String,
    /// Full ref of the group's temporary branch
    /// (`refs/heads/gh-readonly-queue/<base>/pr-<n>-<parent sha>`).
    pub head_ref: String,
    pub head_sha: String,
    /// Full ref of the target branch (`refs/heads/<base>`).
    pub base_ref: String,
    pub base_sha: String,
    /// Git author name of the group's head commit.
    pub author: String,
    /// PR number from the queue ref, if known (0 otherwise).
    pub pr_number: i64,
}

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

    if github_sender.is_none() {
        warn!("GitHub service is down, unable to service merge_group webhook. Restart Eka-CI");
        return;
    }
    if merge_group_payload.action != MGWEA::ChecksRequested {
        debug!(
            "Ignoring merge_group action: {:?}",
            merge_group_payload.action
        );
        return;
    }
    let Some(group) = parse_merge_group(merge_group_payload, repository_info) else {
        return;
    };
    debug!(
        "Processing merge queue commit {} for ref {} in {}/{}",
        group.head_sha, group.head_ref, group.owner, group.repo_name
    );

    if !merge_group_permitted(&group, &github_app_configs) {
        return;
    }
    if merge_queue_require_approval && !merge_group_author_approved(&group, &db_service).await {
        return;
    }
    if let Err(e) = queue_merge_group_evaluation(&group, false, &git_sender, &db_service).await {
        warn!(
            "Failed to queue evaluation of merge queue commit {}: {:?}",
            group.head_sha, e
        );
    }
}

fn parse_merge_group(
    payload: payload::MergeGroupWebhookEventPayload,
    repository_info: Option<(String, String)>,
) -> Option<MergeGroup> {
    let Some((owner, repo_name)) = repository_info else {
        warn!("Missing repository info in merge_group webhook");
        return None;
    };
    let data: MergeGroupData = match serde_json::from_value(payload.merge_group) {
        Ok(data) => data,
        Err(e) => {
            warn!("Failed to parse merge_group data: {:?}", e);
            return None;
        },
    };
    Some(MergeGroup {
        owner,
        repo_name,
        head_ref: data.head_ref,
        head_sha: data.head_sha,
        base_ref: data.base_ref,
        base_sha: data.base_sha,
        author: data.head_commit.author.name,
        pr_number: 0,
    })
}

/// Whether any configured GitHub App may act on the group's target branch.
pub(crate) fn merge_group_permitted(
    group: &MergeGroup,
    github_app_configs: &HashMap<String, GitHubAppConfig>,
) -> bool {
    let permission_context = PermissionContext {
        repo_owner: group.owner.clone(),
        repo_name: group.repo_name.clone(),
        branch: Some(group.base_ref.clone()),
    };
    let has_permission = github_app_configs
        .values()
        .any(|config| check_github_app_permission(config, &permission_context).is_ok());
    if !has_permission {
        warn!(
            "No GitHub App has permission for repository {}/{} branch {}. Skipping merge group \
             for commit {}",
            group.owner, group.repo_name, group.base_ref, group.head_sha
        );
    }
    has_permission
}

/// Whether the group's head commit author is approved for merge-queue
/// builds. Merge groups carry no user id, so only the name is checked.
pub(crate) async fn merge_group_author_approved(
    group: &MergeGroup,
    db_service: &DbService,
) -> bool {
    match db_service.is_user_approved(&group.author, 0).await {
        Ok(approved) => {
            if !approved {
                warn!(
                    "Merge queue build requires approval. User {} is not approved for {}/{}",
                    group.author, group.owner, group.repo_name
                );
            }
            approved
        },
        Err(e) => {
            warn!(
                "Failed to check approval status for user {}: {:?}",
                group.author, e
            );
            false
        },
    }
}

/// Record the merge-queue build, queue the checkout + evaluation of the
/// group's head commit, and record the request for the reconciler.
pub(crate) async fn queue_merge_group_evaluation(
    group: &MergeGroup,
    from_reconciler: bool,
    git_sender: &mpsc::Sender<GitTask>,
    db_service: &DbService,
) -> anyhow::Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = db_service
        .upsert_merge_queue_build(
            &group.owner,
            &group.repo_name,
            &group.head_sha,
            &group.base_sha,
            &group.base_ref,
            &group.head_sha, // merge_group_head_sha is same as head_sha
            &now,
            &now,
        )
        .await
    {
        // Evaluation doesn't depend on the row; keep going.
        warn!(
            "Failed to store merge queue build for commit {}: {:?}",
            group.head_sha, e
        );
    }

    let pr = synthetic_pull_request(group)?;
    git_sender
        .send(GitTask::GitHubCheckout(Box::new(pr)))
        .await
        .context("failed to send merge queue checkout task")?;
    crate::db::github::record_eval_request(
        crate::db::github::HeadCommit {
            owner: &group.owner,
            repo_name: &group.repo_name,
            sha: &group.head_sha,
        },
        group.pr_number,
        from_reconciler,
        &db_service.pool,
    )
    .await
    .context("failed to record merge queue eval request")?;
    debug!(
        "Queued merge queue checkout task for commit {}",
        group.head_sha
    );
    Ok(())
}

/// Build a synthetic `PullRequest` (head = group commit, base = target
/// branch) so the regular `GitTask::GitHubCheckout` flow evaluates it.
///
/// octocrab's models have many required fields; only the repository
/// (`html_url` domain, owner login, name), refs and SHAs are read by the
/// checkout and CI paths, the rest are placeholders.
fn synthetic_pull_request(group: &MergeGroup) -> anyhow::Result<PullRequest> {
    let repo_url = format!("https://github.com/{}/{}", group.owner, group.repo_name);
    let repo = serde_json::json!({
        "id": 0,
        "name": group.repo_name,
        "full_name": format!("{}/{}", group.owner, group.repo_name),
        "url": format!("https://api.github.com/repos/{}/{}", group.owner, group.repo_name),
        "html_url": repo_url,
        "clone_url": format!("{repo_url}.git"),
        "owner": synthetic_author(&group.owner),
    });
    let pr_value = serde_json::json!({
        "url": repo_url,
        "id": 0,
        "number": 0, // Merge queue doesn't have a PR number in this context
        "locked": false,
        "maintainer_can_modify": false,
        "state": "open",
        "title": format!("Merge queue for {}", group.base_ref),
        "head": { "ref": group.head_ref, "sha": group.head_sha, "repo": repo },
        "base": { "ref": group.base_ref, "sha": group.base_sha, "repo": repo },
    });
    serde_json::from_value(pr_value).context("failed to build synthetic merge queue PullRequest")
}

/// A placeholder GitHub user object carrying only a real `login`.
fn synthetic_author(login: &str) -> serde_json::Value {
    let url = format!("https://github.com/{login}");
    serde_json::json!({
        "login": login,
        "id": 0,
        "node_id": "",
        "avatar_url": url,
        "gravatar_id": "",
        "url": url,
        "html_url": url,
        "followers_url": url,
        "following_url": url,
        "gists_url": url,
        "starred_url": url,
        "subscriptions_url": url,
        "organizations_url": url,
        "repos_url": url,
        "events_url": url,
        "received_events_url": url,
        "type": "Organization",
        "site_admin": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_pull_request_carries_group_commits() {
        let group = MergeGroup {
            owner: "o".into(),
            repo_name: "r".into(),
            head_ref: "refs/heads/gh-readonly-queue/main/pr-7-aaa".into(),
            head_sha: "head".into(),
            base_ref: "refs/heads/main".into(),
            base_sha: "base".into(),
            author: "someone".into(),
            pr_number: 7,
        };
        let pr = synthetic_pull_request(&group).unwrap();
        assert_eq!(pr.head.sha, "head");
        assert_eq!(pr.base.sha, "base");
        assert_eq!(pr.head.ref_field, group.head_ref);
        let head = crate::github::CICheckInfo::from_gh_pr_head(&pr).unwrap();
        assert_eq!((head.owner.as_str(), head.repo_name.as_str()), ("o", "r"));
        assert_eq!(head.base_commit.as_deref(), Some("base"));
        let repo = git::GitRepo::from_gh_repo(pr.base.repo.unwrap()).unwrap();
        assert_eq!(repo.checkout_url(), "https://github.com/o/r.git");
    }
}
