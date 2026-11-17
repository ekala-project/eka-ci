//! Reconciliation of live merge-queue groups.
//!
//! GitHub's REST API has no endpoint listing merge groups, but each live
//! group is backed by a temporary branch
//! `gh-readonly-queue/<base branch>/pr-<number>-<parent sha>` that exists
//! until the group merges or is invalidated. Listing those refs gives
//! every group head that may still need checks.

use anyhow::{Context, Result};
use octocrab::Octocrab;
use serde::Deserialize;
use tracing::warn;

use super::{Decision, EvalReconciler, log_enqueue};
use crate::db::github::{HeadCommit, RepositoryInfo};
use crate::github::webhook::merge_queue::{
    MergeGroup, merge_group_author_approved, merge_group_permitted, queue_merge_group_evaluation,
};

const QUEUE_REF_PREFIX: &str = "refs/heads/gh-readonly-queue/";

#[derive(Debug, Deserialize)]
struct GitRef {
    #[serde(rename = "ref")]
    name: String,
    object: GitObject,
}

#[derive(Debug, Deserialize)]
struct GitObject {
    sha: String,
}

/// A merge-queue ref, parsed from its name.
#[derive(Debug, PartialEq, Eq)]
struct QueueRef<'a> {
    base_branch: &'a str,
    pr_number: i64,
}

/// Parse `refs/heads/gh-readonly-queue/<base>/pr-<n>-<sha>`. The base
/// branch may itself contain `/`.
fn parse_queue_ref(name: &str) -> Option<QueueRef<'_>> {
    let rest = name.strip_prefix(QUEUE_REF_PREFIX)?;
    let (base_branch, group) = rest.rsplit_once("/pr-")?;
    let (number, _parent_sha) = group.split_once('-')?;
    Some(QueueRef {
        base_branch,
        pr_number: number.parse().ok()?,
    })
}

impl EvalReconciler {
    /// Queue an evaluation for every live merge group whose head commit
    /// was never evaluated.
    pub(super) async fn reconcile_merge_groups(
        &self,
        octocrab: &Octocrab,
        repo: &RepositoryInfo,
    ) -> Result<()> {
        let route = format!(
            "/repos/{}/{}/git/matching-refs/heads/gh-readonly-queue/",
            repo.owner, repo.repo_name
        );
        let refs: Vec<GitRef> = octocrab
            .get(route, None::<&()>)
            .await
            .context("failed to list merge queue refs")?;
        for git_ref in refs {
            if let Err(e) = self.reconcile_merge_group(octocrab, repo, &git_ref).await {
                warn!(
                    "Failed to reconcile merge group {} of {}/{}: {:?}",
                    git_ref.name, repo.owner, repo.repo_name, e
                );
            }
        }
        Ok(())
    }

    async fn reconcile_merge_group(
        &self,
        octocrab: &Octocrab,
        repo: &RepositoryInfo,
        git_ref: &GitRef,
    ) -> Result<()> {
        let Some(queue_ref) = parse_queue_ref(&git_ref.name) else {
            warn!("Unrecognized merge queue ref {}", git_ref.name);
            return Ok(());
        };
        let head = HeadCommit {
            owner: &repo.owner,
            repo_name: &repo.repo_name,
            sha: &git_ref.object.sha,
        };
        let (decision, request) = self.head_decision(head).await?;
        if decision != Decision::Enqueue {
            return Ok(());
        }

        let group = self
            .load_merge_group(octocrab, repo, git_ref, &queue_ref)
            .await?;
        if !merge_group_permitted(&group, &self.github_app_configs) {
            return Ok(());
        }
        if self.approval.merge_queue && !merge_group_author_approved(&group, &self.db_service).await
        {
            return Ok(());
        }
        let what = format!("merge group for #{}", queue_ref.pr_number);
        log_enqueue(head, &what, request.as_ref());
        queue_merge_group_evaluation(&group, true, &self.git_sender, &self.db_service).await
    }

    /// Fill in the group's parent commit and author from its head commit.
    async fn load_merge_group(
        &self,
        octocrab: &Octocrab,
        repo: &RepositoryInfo,
        git_ref: &GitRef,
        queue_ref: &QueueRef<'_>,
    ) -> Result<MergeGroup> {
        let route = format!(
            "/repos/{}/{}/commits/{}",
            repo.owner, repo.repo_name, git_ref.object.sha
        );
        let commit: serde_json::Value = octocrab
            .get(route, None::<&()>)
            .await
            .context("failed to fetch merge group head commit")?;
        let base_sha = commit["parents"][0]["sha"]
            .as_str()
            .context("merge group head commit has no parent")?;
        let author = commit["commit"]["author"]["name"]
            .as_str()
            .unwrap_or_default();
        Ok(MergeGroup {
            owner: repo.owner.clone(),
            repo_name: repo.repo_name.clone(),
            head_ref: git_ref.name.clone(),
            head_sha: git_ref.object.sha.clone(),
            base_ref: format!("refs/heads/{}", queue_ref.base_branch),
            base_sha: base_sha.to_string(),
            author: author.to_string(),
            pr_number: queue_ref.pr_number,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_queue_refs() {
        let parsed = parse_queue_ref("refs/heads/gh-readonly-queue/main/pr-7-0123abcd");
        assert_eq!(
            parsed,
            Some(QueueRef {
                base_branch: "main",
                pr_number: 7
            })
        );
    }

    #[test]
    fn parses_base_branches_with_slashes() {
        let parsed = parse_queue_ref("refs/heads/gh-readonly-queue/release/26.11/pr-12-ff00");
        assert_eq!(
            parsed,
            Some(QueueRef {
                base_branch: "release/26.11",
                pr_number: 12
            })
        );
    }

    #[test]
    fn rejects_other_refs() {
        assert_eq!(parse_queue_ref("refs/heads/main"), None);
        assert_eq!(
            parse_queue_ref("refs/heads/gh-readonly-queue/main/pr-x-ff00"),
            None
        );
    }
}
