// Main build event request handler

use std::sync::Arc;

use tracing::{debug, warn};

use super::{RecorderTask, RecorderWorker};
use crate::db::model::build_event;

impl RecorderWorker {
    pub(super) async fn handle_recorder_request(&self, task: &RecorderTask) -> anyhow::Result<()> {
        use build_event::*;
        use {DrvBuildResult as DBR, DrvBuildState as DBS};

        let drv = &task.derivation;

        // Derive build_attempt from current state: FailedRetry means this
        // is the second attempt, everything else is the first.
        let current_state = self.db_service.get_drv(drv).await?.map(|d| d.build_state);
        let attempt = match &current_state {
            Some(DBS::FailedRetry) => std::num::NonZeroU32::new(2).unwrap(),
            _ => std::num::NonZeroU32::new(1).unwrap(),
        };

        let build_id = crate::db::model::build::DrvBuildId {
            derivation: (**drv).clone(),
            build_attempt: attempt,
        };

        let job_infos = self.db_service.get_job_info_for_drv(drv).await?;
        let mut size_check: Option<bool> = None;

        match &task.result {
            DBS::Completed(DBR::Success) => {
                debug!(
                    "Attempting to record successful build of {}",
                    build_id.derivation.store_path()
                );
                let old_state = current_state.clone().unwrap_or(DBS::Queued);

                self.update_and_broadcast(drv, &old_state, &task.result)
                    .await?;

                // Only run post-build hooks, size checks, and runtime
                // reference capture when the output was actually built
                // locally. Substitution cache-hits go directly from
                // Queued → Completed(Success) without passing through
                // Building, so we skip expensive nix path-info calls
                // for those (outputs aren't guaranteed to be local).
                let was_built_locally = matches!(old_state, DBS::Building);

                if was_built_locally {
                    // Execute post-build hooks if configured
                    if let Err(e) = self.execute_hooks_for_drv(drv).await {
                        warn!("Failed to execute hooks for {}: {}", drv.store_path(), e);
                    }

                    // Capture runtime references for dependency tracking
                    if let Err(e) = self.capture_runtime_references(drv, &job_infos).await {
                        warn!(
                            "Failed to capture runtime references for {}: {}",
                            drv.store_path(),
                            e
                        );
                    }
                }
                // Sizes are measured and the build reported to its check
                // runs in the background once the forge has been notified
                // (below). A cache hit is reported only if it is a job.
                if was_built_locally || !job_infos.is_empty() {
                    size_check = Some(was_built_locally);
                }

                // Clear transitive failures in database first (source of
                // truth for crash recovery), then update the in-memory
                // graph. This order ensures the DB is never behind the
                // graph — if the graph update fails, restart self-heals.
                self.db_service.clear_transitive_failures(drv).await?;
                let unblocked_drvs = self.clear_graph_failure(drv).await?;

                // Re-check drvs that were unblocked
                for unblocked_drv in &unblocked_drvs {
                    self.buildability.push(Arc::new(unblocked_drv.clone()));
                }

                // Check direct referrers for buildability.
                // Uses the shared_view directly instead of the graph
                // command channel to avoid blocking when the channel
                // is saturated by the BFS cascade.
                let shared_drv_id = crate::graph_compat::to_shared_drv_id(drv)?;
                let referrers = self.graph_handle.get_dependents_from_view(&shared_drv_id);
                for referrer in referrers {
                    let server_referrer = crate::graph_compat::to_server_drv_id(&referrer)?;
                    self.buildability.push(Arc::new(server_referrer));
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
                    DBS::Building => {
                        // First failure - transition to FailedRetry and re-queue immediately
                        debug!(
                            "First failure for {}, transitioning to FailedRetry",
                            drv.store_path()
                        );
                        let old_state = current_drv.build_state.clone();
                        self.update_and_broadcast(drv, &old_state, &DBS::FailedRetry)
                            .await?;

                        // Re-queue immediately for retry
                        self.buildability.push(Arc::clone(drv));
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

                        // Propagate failure: graph first to discover
                        // blocked drvs, then persist to DB. The graph
                        // BFS is the only way to find transitively
                        // blocked nodes, so it must run first here.
                        // The DB insert that follows persists the
                        // result; if it fails, the graph and DB diverge
                        // but restart will re-propagate from the
                        // terminal failure state stored in DB.
                        let blocked_drvs = self.propagate_graph_failure(drv).await?;

                        if !blocked_drvs.is_empty() {
                            self.db_service
                                .insert_transitive_failures(drv, &blocked_drvs)
                                .await?;
                        }
                    },
                    _ if current_drv.build_state.is_terminal() => {
                        // Stale recorder event — drv already reached a terminal state.
                        // Skip to avoid reverting a finalized state.
                        debug!(
                            "Ignoring stale failure for {} (already {:?})",
                            drv.store_path(),
                            current_drv.build_state
                        );
                    },
                    _ => {
                        warn!(
                            "Unexpected state {:?} when recording failure for {}",
                            current_drv.build_state,
                            drv.store_path()
                        );
                    },
                }
            },
            DBS::Interrupted(ref kind) => {
                let current_drv = self
                    .db_service
                    .get_drv(drv)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("Drv not found: {}", drv.store_path()))?;

                // Guard: skip stale events for drvs already in terminal state.
                if current_drv.build_state.is_terminal() {
                    debug!(
                        "Ignoring stale interruption for {} (already {:?})",
                        drv.store_path(),
                        current_drv.build_state
                    );
                } else if kind.is_retryable() {
                    let old_state = current_drv.build_state.clone();
                    // Retryable interruption (Timeout, OOM, ProcessDeath).
                    // Same two-attempt budget as Completed(Failure):
                    //   Building -> first interrupt  -> FailedRetry (re-queue)
                    //   FailedRetry -> second interrupt -> Completed(Failure) + propagate
                    match current_drv.build_state {
                        DBS::FailedRetry => {
                            debug!(
                                "Second interruption ({:?}) for {}, marking as permanent failure",
                                kind,
                                drv.store_path()
                            );
                            self.update_and_broadcast(
                                drv,
                                &old_state,
                                &DBS::Completed(DBR::Failure),
                            )
                            .await?;

                            let blocked_drvs = self.propagate_graph_failure(drv).await?;
                            if !blocked_drvs.is_empty() {
                                self.db_service
                                    .insert_transitive_failures(drv, &blocked_drvs)
                                    .await?;
                            }
                        },
                        _ => {
                            debug!(
                                "Retryable interruption ({:?}) for {}, transitioning to \
                                 FailedRetry",
                                kind,
                                drv.store_path()
                            );
                            self.update_and_broadcast(drv, &old_state, &DBS::FailedRetry)
                                .await?;

                            self.buildability.push(Arc::clone(drv));
                        },
                    }
                } else {
                    let old_state = current_drv.build_state.clone();
                    // Non-retryable interruption (Cancelled, SchedulerDeath).
                    // Record interrupted state and propagate TransitiveFailure.
                    debug!(
                        "Non-retryable interruption ({:?}) for {}, propagating TransitiveFailure",
                        kind,
                        drv.store_path()
                    );
                    self.update_and_broadcast(drv, &old_state, &task.result)
                        .await?;

                    let blocked_drvs = self.propagate_graph_failure(drv).await?;
                    if !blocked_drvs.is_empty() {
                        self.db_service
                            .insert_transitive_failures(drv, &blocked_drvs)
                            .await?;
                    }
                }
            },
            DBS::UnsatisfiableRequirements => {
                let old_state = self
                    .db_service
                    .get_drv(drv)
                    .await?
                    .map(|d| d.build_state)
                    .unwrap_or(DBS::Queued);

                if old_state.is_terminal() {
                    debug!(
                        "Ignoring stale UnsatisfiableRequirements for {} (already {:?})",
                        drv.store_path(),
                        old_state
                    );
                } else {
                    self.update_and_broadcast(drv, &old_state, &task.result)
                        .await?;

                    let blocked_drvs = self.propagate_graph_failure(drv).await?;
                    if !blocked_drvs.is_empty() {
                        self.db_service
                            .insert_transitive_failures(drv, &blocked_drvs)
                            .await?;
                    }
                }
            },
            _ => {
                warn!(
                    "Unexpected recorder task state {:?} for {}",
                    task.result,
                    drv.store_path()
                );
            },
        }

        self.notify_forge_and_channels(drv, task, &job_infos)
            .await?;

        // Spawned only now, so the build report is queued after the plain
        // success update above and is not overwritten by it.
        if let Some(built_locally) = size_check {
            self.size_checker
                .spawn(Arc::clone(drv), job_infos.clone(), built_locally);
        }

        // Broadcast job stats updates for all affected jobs
        for job_info in job_infos {
            self.broadcast_job_stats(job_info.jobset_id).await;
        }

        Ok(())
    }
}
