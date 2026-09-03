// Forge (GitHub) and channel notification logic for build events

use std::sync::Arc;

use tracing::{debug, warn};

use super::{RecorderTask, RecorderWorker};
use crate::channels::types::ChannelTask;
use crate::config::ChannelForge;
use crate::db::model::{build_event, drv_id};
use crate::github::GitHubTask;
use crate::scheduler::ingress::IngressTask;

impl RecorderWorker {
    /// Send GitHub check-run updates, jobset completion notifications, and
    /// channel/auto-merge signals after a build state change.
    pub(super) async fn notify_forge_and_channels(
        &self,
        drv: &Arc<drv_id::DrvId>,
        task: &RecorderTask,
        job_infos: &[crate::db::github::JobInfo],
    ) -> anyhow::Result<()> {
        use build_event::*;
        use {DrvBuildResult as DBR, DrvBuildState as DBS};

        let Some(github_sender) = &self.github_sender else {
            return Ok(());
        };

        // Create check_runs for new failures that don't have one yet
        if task.result.is_failure() {
            let existing_check_runs = self.db_service.check_runs_for_drv_path(drv).await?;

            if existing_check_runs.is_empty() {
                let job_infos = self.db_service.get_job_info_for_drv(drv).await?;

                for job_info in job_infos {
                    let create_task = GitHubTask::CreateFailureCheckRun {
                        drv_id: Arc::clone(drv),
                        jobset_id: job_info.jobset_id,
                        job_attr_name: job_info.name.clone(),
                        difference: job_info.difference,
                    };
                    if let Err(e) = github_sender.send(create_task).await {
                        warn!(
                            "Failed to send CreateFailureCheckRun for {}: {:?}",
                            drv.store_path(),
                            e
                        );
                    }
                }
            }
        }

        // Only send UpdateBuildStatus if the drv has check_runs.
        // Dependency drvs (from the BFS cascade) rarely have
        // check_runs — skipping them avoids flooding the GitHub
        // service channel with no-op tasks.
        let has_check_runs = !self
            .db_service
            .check_runs_for_drv_path(drv)
            .await?
            .is_empty();
        if has_check_runs {
            let github_task = GitHubTask::UpdateBuildStatus {
                drv_id: Arc::clone(drv),
                status: task.result.clone(),
            };
            if let Err(e) = github_sender.send(github_task).await {
                warn!(
                    "Failed to send GitHub update for {}: {:?}",
                    drv.store_path(),
                    e
                );
            }
        }

        // When a drv with check_runs completes, re-dispatch
        // other queued drvs in the same jobset(s) for build
        // re-evaluation. This handles the cascade when icu78
        // completes and its dependents become buildable.
        if has_check_runs && task.result == DBS::Completed(DBR::Success) {
            for job_info in job_infos {
                let queued_jobs = self
                    .db_service
                    .get_new_or_changed_jobs(job_info.jobset_id)
                    .await?;
                for queued_job in queued_jobs {
                    // Only re-dispatch drvs that are still queued
                    if let Some(drv_entry) = self.db_service.get_drv(&queued_job.drv_path).await? {
                        if matches!(drv_entry.build_state, DBS::Queued) {
                            let drv_id = Arc::new(queued_job.drv_path);
                            if let Err(e) = self
                                .ingress_sender
                                .try_send(IngressTask::EvalRequest(drv_id))
                            {
                                warn!("ingress queue full, dropped EvalRequest re-dispatch: {}", e);
                            }
                        }
                    }
                }
            }
        }

        // Check if this drv completion concludes any jobsets
        // Only check if we've reached a terminal state
        if task.result.is_terminal() {
            self.check_jobset_completion(drv, github_sender).await?;
        }

        Ok(())
    }

    /// Check if any jobsets are now fully concluded and send completion
    /// notifications (GitHub check-run completion, channel events, auto-merge).
    async fn check_jobset_completion(
        &self,
        drv: &Arc<drv_id::DrvId>,
        github_sender: &tokio::sync::mpsc::Sender<GitHubTask>,
    ) -> anyhow::Result<()> {
        let job_infos = self.db_service.get_job_info_for_drv(drv).await?;

        for job_info in job_infos {
            if !self
                .db_service
                .all_jobs_concluded(job_info.jobset_id)
                .await?
            {
                continue;
            }

            let has_failures = self
                .db_service
                .jobset_has_new_or_changed_failures(job_info.jobset_id)
                .await?;

            let conclusion = if has_failures {
                octocrab::params::checks::CheckRunConclusion::Failure
            } else {
                octocrab::params::checks::CheckRunConclusion::Success
            };

            let jobset_info = self.db_service.get_jobset_info(job_info.jobset_id).await?;

            let complete_task = GitHubTask::CompleteCIEvalJob {
                ci_check_info: Arc::new(crate::github::CICheckInfo {
                    commit: jobset_info.sha.clone(),
                    base_commit: None,
                    owner: jobset_info.owner.clone(),
                    repo_name: jobset_info.repo_name.clone(),
                }),
                job_name: jobset_info.job.clone(),
                conclusion: conclusion.into(),
            };

            if let Err(e) = github_sender.send(complete_task).await {
                warn!(
                    "Failed to send CompleteCIEvalJob for jobset {}: {:?}",
                    job_info.jobset_id, e
                );
            }

            // Notify ChannelService that this jobset has concluded.
            // Today the recorder only sees GitHub-backed jobsets, so
            // the forge is hard-coded to GitHub.
            if let Some(channel_sender) = &self.channel_sender {
                let channel_task = ChannelTask::JobsetComplete {
                    forge: ChannelForge::GitHub,
                    owner: jobset_info.owner.clone(),
                    repo: jobset_info.repo_name.clone(),
                    sha: jobset_info.sha.clone(),
                };
                if let Err(e) = channel_sender.send(channel_task).await {
                    warn!(
                        "Failed to send ChannelTask::JobsetComplete for jobset {}: {:?}",
                        job_info.jobset_id, e
                    );
                }
            }

            // Check if this is a PR that should be auto-merged
            if !has_failures {
                self.check_auto_merge(&jobset_info, github_sender).await?;
            }
        }

        Ok(())
    }

    /// Check if a PR associated with a jobset should be auto-merged.
    async fn check_auto_merge(
        &self,
        jobset_info: &crate::db::github::JobSetInfo,
        github_sender: &tokio::sync::mpsc::Sender<GitHubTask>,
    ) -> anyhow::Result<()> {
        if let Ok(Some(pr)) = crate::db::github::get_pr_by_head_sha(
            &jobset_info.sha,
            &jobset_info.owner,
            &jobset_info.repo_name,
            &self.db_service.pool,
        )
        .await
        {
            // Fire CheckAutoMerge if UI auto-merge is on or a
            // comment-merge is pending. The handler re-validates all
            // gates (SHA-drift included), so over-triggering is safe.
            let has_pending_comment_merge = pr.comment_merge_sha.is_some();
            let is_open = pr.state == "open";
            if is_open && (pr.auto_merge_enabled || has_pending_comment_merge) {
                debug!(
                    "PR #{} eligible for auto-merge check (auto_merge={}, \
                     comment_merge_pending={}), scheduling",
                    pr.pr_number, pr.auto_merge_enabled, has_pending_comment_merge
                );

                let auto_merge_task = GitHubTask::CheckAutoMerge {
                    owner: jobset_info.owner.clone(),
                    repo_name: jobset_info.repo_name.clone(),
                    pr_number: pr.pr_number,
                };

                if let Err(e) = github_sender.send(auto_merge_task).await {
                    warn!(
                        "Failed to send CheckAutoMerge for PR #{}: {:?}",
                        pr.pr_number, e
                    );
                }
            }
        }

        Ok(())
    }
}
