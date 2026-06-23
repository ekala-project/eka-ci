// Main build event request handler

use std::sync::Arc;

use tracing::{debug, warn};

use super::{RecorderTask, RecorderWorker};
use crate::channels::types::ChannelTask;
use crate::config::ChannelForge;
use crate::db::model::build_event;
use crate::github::GitHubTask;
use crate::scheduler::ingress::IngressTask;

impl RecorderWorker {
    pub(super) async fn handle_recorder_request(&self, task: &RecorderTask) -> anyhow::Result<()> {
        use build_event::*;
        use {DrvBuildResult as DBR, DrvBuildState as DBS};

        let drv = &task.derivation;
        let build_id = crate::db::model::build::DrvBuildId {
            // `DrvBuildId` stores an owned `DrvId`; Arc deref + clone.
            derivation: (**drv).clone(),
            // TODO: build_attempt seems like something we should query
            build_attempt: std::num::NonZeroU32::new(1).unwrap(),
        };

        let job_infos = self.db_service.get_job_info_for_drv(drv).await?;

        match &task.result {
            DBS::Completed(DBR::Success) => {
                debug!(
                    "Attempting to record successful build of {}",
                    build_id.derivation.store_path()
                );
                // Get old state before updating
                let old_state = self
                    .db_service
                    .get_drv(drv)
                    .await?
                    .map(|d| d.build_state)
                    .unwrap_or(DBS::Queued);

                self.update_and_broadcast(drv, &old_state, &task.result)
                    .await?;

                // Execute post-build hooks if configured
                if let Err(e) = self.execute_hooks_for_drv(drv).await {
                    warn!("Failed to execute hooks for {}: {}", drv.store_path(), e);
                    // Don't fail the build if hooks fail - they run asynchronously
                }

                // Capture runtime references for dependency tracking first so
                // that subsequent per-output size updates have rows to land on.
                if let Err(e) = self.capture_runtime_references(drv, &job_infos).await {
                    warn!(
                        "Failed to capture runtime references for {}: {}",
                        drv.store_path(),
                        e
                    );
                    // Don't fail the build if runtime ref capture fails
                }

                // Calculate and check output size if configured
                if let Err(e) = self.check_output_size(drv, &job_infos).await {
                    warn!(
                        "Failed to check output size for {}: {}",
                        drv.store_path(),
                        e
                    );
                    // Don't fail the build if size check fails
                }

                // TODO: closure size will be a future feature
                // if let Err(e) = self.check_closure_size(drv, &job_infos).await {
                //     warn!(
                //         "Failed to check closure size for {}: {}",
                //         drv.store_path(),
                //         e
                //     );
                //     // Don't fail the build if closure size check fails
                // }

                // Clear any transitive failures in graph (fast in-memory operation)
                let unblocked_drvs = self.clear_graph_failure(drv).await?;

                // Also clear in database for persistence
                self.db_service.clear_transitive_failures(drv).await?;

                // Re-queue drvs that were unblocked
                for unblocked_drv in unblocked_drvs {
                    let task = IngressTask::CheckBuildable(std::sync::Arc::new(unblocked_drv));
                    self.ingress_sender.send(task).await?;
                }

                // Check direct referrers for buildability
                let shared_drv_id = crate::graph_compat::to_shared_drv_id(drv)?;
                let referrers = self.graph_handle.get_dependents(&shared_drv_id).await?;
                for referrer in referrers {
                    let server_referrer = crate::graph_compat::to_server_drv_id(&referrer)?;
                    let task = IngressTask::CheckBuildable(std::sync::Arc::new(server_referrer));
                    self.ingress_sender.send(task).await?;
                }
            },
            DBS::Completed(DBR::Failure) => {
                debug!(
                    "Attempting to record failed build of {}",
                    build_id.derivation.store_path()
                );

                // Check current state to determine if this is first or second failure
                let current_drv = self
                    .db_service
                    .get_drv(drv)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Drv not found: {}", drv.store_path()))?;

                match current_drv.build_state {
                    DBS::Buildable => {
                        // First failure - transition to FailedRetry and re-queue immediately
                        debug!(
                            "First failure for {}, transitioning to FailedRetry",
                            drv.store_path()
                        );
                        let old_state = current_drv.build_state.clone();
                        self.update_and_broadcast(drv, &old_state, &DBS::FailedRetry)
                            .await?;

                        // Re-queue immediately for retry
                        let task = IngressTask::CheckBuildable(Arc::clone(drv));
                        self.ingress_sender.send(task).await?;
                    },
                    DBS::FailedRetry => {
                        // Second failure - permanent failure, propagate to downstream
                        debug!(
                            "Second failure for {}, marking as permanent failure",
                            drv.store_path()
                        );
                        let old_state = current_drv.build_state.clone();
                        self.update_and_broadcast(drv, &old_state, &task.result)
                            .await?;

                        // Propagate failure in graph (fast in-memory BFS traversal)
                        let blocked_drvs = self.propagate_graph_failure(drv).await?;

                        // Also propagate in database for persistence
                        if !blocked_drvs.is_empty() {
                            self.db_service
                                .insert_transitive_failures(drv, &blocked_drvs)
                                .await?;
                        }
                    },
                    _ => {
                        // Unexpected state - log warning but still record failure
                        warn!(
                            "Unexpected state {:?} when recording failure for {}",
                            current_drv.build_state,
                            drv.store_path()
                        );
                        let old_state = current_drv.build_state.clone();
                        self.update_and_broadcast(drv, &old_state, &task.result)
                            .await?;
                    },
                }
            },
            _ => {},
        }

        if let Some(github_sender) = &self.github_sender {
            // Check if we need to create check_runs for failures
            // Only create check_runs if the build failed and no check_run exists yet
            if task.result.is_failure() {
                let existing_check_runs = self.db_service.check_runs_for_drv_path(drv).await?;

                if existing_check_runs.is_empty() {
                    // No check_run exists, we need to create one
                    // Get job info to know which jobsets this drv belongs to
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

            // Send update for existing check_runs
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

            // Check if this drv completion concludes any jobsets
            // Only check if we've reached a terminal state
            if task.result.is_terminal() {
                let job_infos = self.db_service.get_job_info_for_drv(drv).await?;

                for job_info in job_infos {
                    // Check if all jobs in this jobset are concluded
                    if self
                        .db_service
                        .all_jobs_concluded(job_info.jobset_id)
                        .await?
                    {
                        // Determine conclusion based on new/changed job failures
                        let has_failures = self
                            .db_service
                            .jobset_has_new_or_changed_failures(job_info.jobset_id)
                            .await?;

                        let conclusion = if has_failures {
                            octocrab::params::checks::CheckRunConclusion::Failure
                        } else {
                            octocrab::params::checks::CheckRunConclusion::Success
                        };

                        // Get jobset info to get the job name
                        let jobset_info =
                            self.db_service.get_jobset_info(job_info.jobset_id).await?;

                        let complete_task = GitHubTask::CompleteCIEvalJob {
                            ci_check_info: Arc::new(crate::github::CICheckInfo {
                                commit: jobset_info.sha.clone(),
                                base_commit: None,
                                owner: jobset_info.owner.clone(),
                                repo_name: jobset_info.repo_name.clone(),
                            }),
                            job_name: jobset_info.job.clone(),
                            conclusion,
                        };

                        if let Err(e) = github_sender.send(complete_task).await {
                            warn!(
                                "Failed to send CompleteCIEvalJob for jobset {}: {:?}",
                                job_info.jobset_id, e
                            );
                        }

                        // Notify ChannelService that this jobset has
                        // concluded. The recorder cannot know which
                        // release channels (if any) care about this
                        // commit, so it always emits; ChannelService
                        // filters by matching `(forge, owner, repo)`
                        // against its registry and discards
                        // jobset_complete events for which no
                        // in-flight Evaluating row exists.
                        //
                        // Today the recorder only sees GitHub-backed
                        // jobsets (the eval service is the sole
                        // producer and is GitHub-only), so the forge
                        // is hard-coded to GitHub. PR 4 will broaden
                        // this when GitLab/Gitea start producing
                        // jobsets.
                        if let Some(channel_sender) = &self.channel_sender {
                            let channel_task = ChannelTask::JobsetComplete {
                                forge: ChannelForge::GitHub,
                                owner: jobset_info.owner.clone(),
                                repo: jobset_info.repo_name.clone(),
                                sha: jobset_info.sha.clone(),
                            };
                            if let Err(e) = channel_sender.send(channel_task).await {
                                warn!(
                                    "Failed to send ChannelTask::JobsetComplete for jobset {}: \
                                     {:?}",
                                    job_info.jobset_id, e
                                );
                            }
                        }

                        // Check if this is a PR that should be auto-merged
                        if !has_failures {
                            // Try to find a PR for this commit
                            if let Ok(Some(pr)) = crate::db::github::get_pr_by_head_sha(
                                &jobset_info.sha,
                                &jobset_info.owner,
                                &jobset_info.repo_name,
                                &self.db_service.pool,
                            )
                            .await
                            {
                                // Fire CheckAutoMerge if UI auto-merge is on or a
                                // comment-merge is pending. The handler re-validates
                                // all gates (SHA-drift included), so over-triggering
                                // is safe — drifted requests cancel cleanly.
                                let has_pending_comment_merge = pr.comment_merge_sha.is_some();
                                let is_open = pr.state == "open";
                                if is_open && (pr.auto_merge_enabled || has_pending_comment_merge) {
                                    debug!(
                                        "PR #{} eligible for auto-merge check (auto_merge={}, \
                                         comment_merge_pending={}), scheduling",
                                        pr.pr_number,
                                        pr.auto_merge_enabled,
                                        has_pending_comment_merge
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
                        }
                    }
                }
            }
        }

        // Broadcast job stats updates for all affected jobs
        for job_info in job_infos {
            self.broadcast_job_stats(job_info.jobset_id).await;
        }

        Ok(())
    }
}
