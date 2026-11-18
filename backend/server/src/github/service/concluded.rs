//! Follow-up for PR jobsets that conclude without a build event.
//!
//! The recorder signals auto-merge when a build concludes a jobset. A
//! jobset with nothing left to build (a docs-only PR, every rebuilt job
//! skipped by the rebuild strategy, or every job already built) never sees
//! such an event, so it is signalled when it is created.

use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::{GitHubService, GitHubTask};
use crate::db::DbService;
use crate::db::github::JobSetInfo;

impl GitHubService {
    /// Signal auto-merge when a newly created PR jobset has already
    /// concluded without failures.
    pub(super) async fn signal_if_concluded(&self, jobset_id: i64) -> anyhow::Result<()> {
        if !self.db_service.all_jobs_concluded(jobset_id).await?
            || self
                .db_service
                .jobset_has_new_or_changed_failures(jobset_id)
                .await?
        {
            return Ok(());
        }
        debug!("Jobset {jobset_id} has nothing left to build; checking auto-merge");
        let jobset_info = self.db_service.get_jobset_info(jobset_id).await?;
        request_auto_merge_check(&self.db_service, &jobset_info, &self.github_sender).await;
        Ok(())
    }
}

/// Send `CheckAutoMerge` for the open PR whose head the jobset evaluated, if
/// UI auto-merge is on or a comment merge is pending. The handler
/// re-validates every gate (SHA drift included), so over-triggering is safe.
pub(crate) async fn request_auto_merge_check(
    db_service: &DbService,
    jobset_info: &JobSetInfo,
    github_sender: &mpsc::Sender<GitHubTask>,
) {
    let pr = match crate::db::github::get_pr_by_head_sha(
        &jobset_info.sha,
        &jobset_info.owner,
        &jobset_info.repo_name,
        &db_service.pool,
    )
    .await
    {
        Ok(Some(pr)) => pr,
        Ok(None) => return,
        Err(e) => {
            warn!(
                "Failed to look up the PR for head {}: {e:#}",
                jobset_info.sha
            );
            return;
        },
    };

    let has_pending_comment_merge = pr.comment_merge_sha.is_some();
    if pr.state != "open" || !(pr.auto_merge_enabled || has_pending_comment_merge) {
        return;
    }
    debug!(
        "PR #{} eligible for auto-merge check (auto_merge={}, comment_merge_pending={}), \
         scheduling",
        pr.pr_number, pr.auto_merge_enabled, has_pending_comment_merge
    );
    let task = GitHubTask::CheckAutoMerge {
        owner: jobset_info.owner.clone(),
        repo_name: jobset_info.repo_name.clone(),
        pr_number: pr.pr_number,
    };
    if let Err(e) = github_sender.send(task).await {
        warn!(
            "Failed to send CheckAutoMerge for PR #{}: {e:?}",
            pr.pr_number
        );
    }
}
