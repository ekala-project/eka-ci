//! Reconciliation of open pull requests.

use anyhow::{Context, Result};
use octocrab::Octocrab;
use octocrab::models::pulls::PullRequest;
use octocrab::params;
use tracing::warn;

use super::{Decision, EvalReconciler, log_enqueue};
use crate::db::github::{HeadCommit, RepositoryInfo};
use crate::github::webhook::pull_requests::{
    is_valid_user, pr_has_app_permission, queue_pr_evaluation, store_pr_metadata,
};

impl EvalReconciler {
    /// Queue an evaluation for every open PR whose head was never evaluated.
    pub(super) async fn reconcile_open_prs(
        &self,
        octocrab: &Octocrab,
        repo: &RepositoryInfo,
    ) -> Result<()> {
        let first_page = octocrab
            .pulls(&repo.owner, &repo.repo_name)
            .list()
            .state(params::State::Open)
            .per_page(100)
            .send()
            .await
            .context("failed to list open PRs")?;
        let prs = octocrab.all_pages(first_page).await?;
        for pr in prs {
            if let Err(e) = self.reconcile_pr(&pr).await {
                warn!(
                    "Failed to reconcile {}/{}#{}: {:?}",
                    repo.owner, repo.repo_name, pr.number, e
                );
            }
        }
        Ok(())
    }

    /// Whether the PR is one the webhook path would have evaluated.
    async fn is_eligible(&self, pr: &PullRequest) -> bool {
        if pr.draft == Some(true) || !pr_has_app_permission(pr, &self.github_app_configs) {
            return false;
        }
        !self.approval.pull_requests || is_valid_user(pr, &self.db_service).await.is_ok()
    }

    async fn reconcile_pr(&self, pr: &PullRequest) -> Result<()> {
        if !self.is_eligible(pr).await {
            return Ok(());
        }
        let head_info = crate::github::CICheckInfo::from_gh_pr_head(pr)?;
        let head = HeadCommit {
            owner: &head_info.owner,
            repo_name: &head_info.repo_name,
            sha: &head_info.commit,
        };
        let (decision, request) = self.head_decision(head).await?;
        if decision != Decision::Enqueue {
            return Ok(());
        }
        log_enqueue(head, &format!("#{}", pr.number), request.as_ref());
        // Webhooks may have been missed entirely; refresh the stored PR.
        store_pr_metadata(pr, "open", &self.db_service).await;
        queue_pr_evaluation(pr, true, &self.git_sender, &self.db_service).await
    }
}
