// ChannelService: orchestrates the release-channel promotion lifecycle.
//
// PR 3 lands the skeleton:
//   - the AsyncService trait impl,
//   - the idempotency guard (terminal decisions for `(channel, sha)`
//     short-circuit re-evaluation),
//   - the coalescer wiring (writes Skipped audit rows for superseded
//     SHAs while a previous evaluation is still in flight), and
//   - a stub evaluator path: job-state snapshotting is a follow-up
//     wired alongside RecorderService -> ChannelService delivery.
//
// The intentional gaps left for PR 4 are marked with NB and isolate
// the cost of the eventual git push integration:
//   - `snapshot_job_states` returns an empty map for now, which keeps
//     evaluation in `Waiting` until the recorder pushes terminal
//     states into this service.
//   - `perform_promotion` only logs; the actual fast-forward push
//     lives in PR 4.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::config::ChannelConfig;
use crate::db::DbService;
use crate::db::model::build_event::DrvBuildState;
use crate::services::AsyncService;

use super::coalescer::{CoalesceAction, coalesce};
use super::evaluator::evaluate_promotion;
use super::types::{ChannelTask, PromotionDecision, PromotionStatus};

/// Capacity of the inbound mpsc channel. Matches other AsyncServices
/// (GitService, EvalService) so backpressure characteristics are
/// uniform across the system.
const CHANNEL_TASK_BUFFER: usize = 1000;

pub struct ChannelService {
    task_sender: mpsc::Sender<ChannelTask>,
    task_receiver: Option<mpsc::Receiver<ChannelTask>>,
    db: DbService,
    /// Per-channel in-memory pending SHA (latest-wins coalescing).
    ///
    /// Lives in a `Mutex` because `handle_task` only borrows `&self`
    /// in the AsyncService dispatch loop. The lock is held very
    /// briefly: just long enough to read + replace one HashMap entry.
    pending: Arc<Mutex<HashMap<String, String>>>,
}

impl ChannelService {
    pub fn new(db: DbService) -> Self {
        let (task_sender, task_receiver) = mpsc::channel(CHANNEL_TASK_BUFFER);
        Self {
            task_sender,
            task_receiver: Some(task_receiver),
            db,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Snapshot the current state of every job named in
    /// `channel.required` and `channel.packages` at `sha`, indexed by
    /// the job name as declared in `.eka-ci/config.json`.
    ///
    /// NB(PR4): this is a stub today. PR 4 will join GitHubJobSets +
    /// Job + the latest DrvBuildEvent for each Drv to produce the
    /// real snapshot. Returning an empty map keeps the evaluator's
    /// decision at `Waiting`, which is the only safe default while
    /// the snapshot is unimplemented.
    async fn snapshot_job_states(
        &self,
        _channel: &ChannelConfig,
        _sha: &str,
    ) -> Result<HashMap<String, DrvBuildState>> {
        Ok(HashMap::new())
    }

    /// Stub: in PR 4 this fast-forwards `target_branch` onto
    /// `tracking_sha` via the appropriate forge API and records
    /// `previous_target_sha`.
    async fn perform_promotion(
        &self,
        channel: &ChannelConfig,
        sha: &str,
    ) -> Result<Option<String>> {
        info!(
            event = "channel_promotion_would_push",
            channel_id = %channel.channel_id(),
            sha = %sha,
            target = %channel.target_branch,
            dry_run = channel.dry_run,
            "PR 3 stub: actual FF push lands in PR 4"
        );
        Ok(None)
    }

    /// Idempotency guard: a `(channel, sha)` pair that has already
    /// reached a terminal decision must not be re-promoted. Replayed
    /// webhooks otherwise would re-drive the entire eval pipeline and
    /// (in PR 4) could attempt a redundant push.
    ///
    /// Returns `true` when the caller should bail because a decision
    /// already exists. A pre-existing `Skipped` row is treated as
    /// non-terminal-for-idempotency: an earlier coalescer skipped this
    /// SHA, and re-arrival means it's the latest again and deserves a
    /// fair evaluation.
    async fn already_decided(&self, channel_id: &str, sha: &str) -> Result<bool> {
        let row = crate::db::channels::get_latest_for_sha(channel_id, sha, &self.db.pool).await?;
        Ok(match row {
            Some(r) => {
                let status = r.status;
                status == PromotionStatus::Promoted.as_i64()
                    || status == PromotionStatus::Blocked.as_i64()
                    || status == PromotionStatus::PushFailed.as_i64()
            },
            None => false,
        })
    }

    async fn handle_evaluate_push(
        &self,
        channel: ChannelConfig,
        sha: String,
    ) -> Result<()> {
        let channel_id = channel.channel_id();

        // 1. Idempotency: shortcut replayed webhooks.
        if self.already_decided(&channel_id, &sha).await? {
            debug!(
                event = "channel_evaluate_idempotent_skip",
                channel_id = %channel_id,
                sha = %sha,
                "terminal decision already recorded; ignoring duplicate event"
            );
            return Ok(());
        }

        // 2. Coalesce against the current in-flight evaluation.
        let in_flight = crate::db::channels::get_in_flight(&channel_id, &self.db.pool).await?;
        let in_flight_sha = in_flight.as_ref().map(|r| r.tracking_sha.as_str());

        let prev_pending: Option<String> = {
            let pending = self.pending.lock().await;
            pending.get(&channel_id).cloned()
        };

        let action = coalesce(in_flight_sha, prev_pending.as_deref(), &sha);

        match action {
            CoalesceAction::AlreadyInFlight => {
                debug!(
                    event = "channel_evaluate_duplicate",
                    channel_id = %channel_id,
                    sha = %sha,
                    "candidate matches in-flight or pending SHA; no-op"
                );
                return Ok(());
            },
            CoalesceAction::DeferAsPending {
                previous_pending_sha,
            } => {
                // Record the superseded pending SHA as an audit row
                // BEFORE updating in-memory state, so a crash between
                // the two leaves the durable history complete.
                if let Some(prev) = previous_pending_sha {
                    crate::db::channels::insert_skipped(
                        &channel_id,
                        &prev,
                        &channel.target_branch,
                        &self.db.pool,
                    )
                    .await?;
                    info!(
                        event = "channel_coalesce_skipped_pending",
                        channel_id = %channel_id,
                        skipped_sha = %prev,
                        new_pending_sha = %sha,
                        "newer SHA superseded an older pending candidate"
                    );
                }
                let mut pending = self.pending.lock().await;
                pending.insert(channel_id.clone(), sha.clone());
                debug!(
                    event = "channel_coalesce_pending",
                    channel_id = %channel_id,
                    pending_sha = %sha,
                    in_flight_sha = ?in_flight_sha,
                    "stored newer SHA as pending; will evaluate when in-flight completes"
                );
                return Ok(());
            },
            CoalesceAction::StartFresh => {
                // Fall through to evaluation below.
            },
        }

        // 3. Open the in-flight Evaluating row. The partial UNIQUE
        // index on the DB column would catch any double-open caused
        // by a race; bubble that up as an anyhow error.
        crate::db::channels::insert_evaluating(
            &channel_id,
            &sha,
            &channel.target_branch,
            &self.db.pool,
        )
        .await?;
        info!(
            event = "channel_evaluation_started",
            channel_id = %channel_id,
            sha = %sha,
            target = %channel.target_branch,
            "opened Evaluating row for channel"
        );

        // 4. Pure-evaluation pass.
        let job_states = self.snapshot_job_states(&channel, &sha).await?;
        let decision = evaluate_promotion(&channel, &job_states);

        // 5. Act on the decision.
        match decision {
            PromotionDecision::Ready { required_results } => {
                // PR 4 will perform the actual FF push here. For now
                // we record the Promoted row so audit history is
                // already structurally correct.
                let previous_target_sha =
                    self.perform_promotion(&channel, &sha).await?;
                let required_json =
                    serde_json::to_string(&required_results).unwrap_or_else(|_| "{}".to_string());
                crate::db::channels::finalise_evaluation(
                    &channel_id,
                    &sha,
                    PromotionStatus::Promoted,
                    None,
                    Some(&required_json),
                    previous_target_sha.as_deref(),
                    &self.db.pool,
                )
                .await?;
                info!(
                    event = "channel_promoted",
                    channel_id = %channel_id,
                    sha = %sha,
                    "channel evaluation reached Promoted (PR3 stub: no actual push performed)"
                );
            },
            PromotionDecision::Blocked {
                failed_required,
                required_results,
            } => {
                let blocked_json = serde_json::json!({
                    "failed_required": failed_required,
                });
                let required_json =
                    serde_json::to_string(&required_results).unwrap_or_else(|_| "{}".to_string());
                crate::db::channels::finalise_evaluation(
                    &channel_id,
                    &sha,
                    PromotionStatus::Blocked,
                    Some(&blocked_json.to_string()),
                    Some(&required_json),
                    None,
                    &self.db.pool,
                )
                .await?;
                warn!(
                    event = "channel_blocked",
                    channel_id = %channel_id,
                    sha = %sha,
                    failed = ?failed_required,
                    "channel evaluation blocked by failed required jobs"
                );
            },
            PromotionDecision::Waiting {
                pending_required,
                pending_packages,
            } => {
                // Leave the Evaluating row in place: a future
                // ChannelTask (e.g. RecorderService telling us a
                // jobset completed) will drive re-evaluation. PR 3
                // intentionally does not promote / fail Waiting rows.
                debug!(
                    event = "channel_evaluation_waiting",
                    channel_id = %channel_id,
                    sha = %sha,
                    pending_required = ?pending_required,
                    pending_packages = ?pending_packages,
                    "channel evaluation waiting on additional job states"
                );
            },
        }

        Ok(())
    }
}

impl AsyncService<ChannelTask> for ChannelService {
    fn get_sender(&self) -> mpsc::Sender<ChannelTask> {
        self.task_sender.clone()
    }

    #[allow(dead_code)] // dispatched via AsyncService::run
    fn take_receiver(&mut self) -> Option<mpsc::Receiver<ChannelTask>> {
        self.task_receiver.take()
    }

    async fn handle_task(&self, task: ChannelTask) -> Result<()> {
        match task {
            ChannelTask::EvaluatePush { channel, sha } => {
                self.handle_evaluate_push(channel, sha).await
            },
        }
    }

    async fn handle_failure(&mut self, error: anyhow::Error) {
        warn!(
            event = "channel_service_task_failed",
            error = ?error,
            "ChannelService task failed; continuing to next task"
        );
    }

    async fn handle_closure(&mut self) {
        info!(
            event = "channel_service_shutdown",
            "ChannelService shutdown requested; draining"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::types::PromotionStatus;
    use crate::config::{ChannelConfig, ChannelForge};
    use crate::db::DbService;

    fn channel(name: &str, required: &[&str], packages: &[&str]) -> ChannelConfig {
        ChannelConfig {
            forge: ChannelForge::GitHub,
            owner: "ekacorp".to_string(),
            repo: "ekapkgs".to_string(),
            name: name.to_string(),
            tracking_branch: "master".to_string(),
            target_branch: format!("{name}-unstable"),
            required: required.iter().map(|s| s.to_string()).collect(),
            packages: packages.iter().map(|s| s.to_string()).collect(),
            dry_run: false,
        }
    }

    #[tokio::test]
    async fn fresh_push_with_empty_required_promotes_immediately() {
        // With required=[] and packages=[] the pure evaluator returns
        // Ready; the service should record a Promoted row. (PR 3
        // stub: no actual FF push.)
        let db = DbService::new_in_memory().await.unwrap();
        let svc = ChannelService::new(db.clone());
        let ch = channel("stable", &[], &[]);
        svc.handle_evaluate_push(ch.clone(), "sha-1".to_string())
            .await
            .unwrap();
        let n = crate::db::channels::count_by_status(
            &ch.channel_id(),
            PromotionStatus::Promoted,
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn fresh_push_with_required_job_waits() {
        // The stub `snapshot_job_states` returns an empty map, so a
        // required job is treated as not-yet-terminal => Waiting,
        // which leaves the Evaluating row in place.
        let db = DbService::new_in_memory().await.unwrap();
        let svc = ChannelService::new(db.clone());
        let ch = channel("stable", &["coreutils"], &[]);
        svc.handle_evaluate_push(ch.clone(), "sha-w".to_string())
            .await
            .unwrap();
        let n_eval = crate::db::channels::count_by_status(
            &ch.channel_id(),
            PromotionStatus::Evaluating,
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(n_eval, 1);
    }

    #[tokio::test]
    async fn duplicate_push_is_idempotent_after_terminal_decision() {
        // First push reaches Promoted (empty required). A second
        // delivery of the same SHA must NOT open a new Evaluating row.
        let db = DbService::new_in_memory().await.unwrap();
        let svc = ChannelService::new(db.clone());
        let ch = channel("stable", &[], &[]);
        svc.handle_evaluate_push(ch.clone(), "sha-dup".to_string())
            .await
            .unwrap();
        svc.handle_evaluate_push(ch.clone(), "sha-dup".to_string())
            .await
            .unwrap();
        let n = crate::db::channels::count_by_status(
            &ch.channel_id(),
            PromotionStatus::Promoted,
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(n, 1, "idempotency guard must prevent duplicate promotion");
    }

    #[tokio::test]
    async fn newer_sha_arriving_during_wait_records_skipped_for_displaced_pending() {
        // sha-a evaluates and goes Waiting (required job).
        // sha-b arrives -> stored as pending (no prior pending => no Skipped row yet).
        // sha-c arrives -> sha-b is now stale and must be audited as Skipped.
        let db = DbService::new_in_memory().await.unwrap();
        let svc = ChannelService::new(db.clone());
        let ch = channel("stable", &["coreutils"], &[]);

        svc.handle_evaluate_push(ch.clone(), "sha-a".to_string())
            .await
            .unwrap();
        // sha-a is now Evaluating (Waiting).
        svc.handle_evaluate_push(ch.clone(), "sha-b".to_string())
            .await
            .unwrap();
        // sha-b deferred, no skipped yet.
        let n_skipped_after_b = crate::db::channels::count_by_status(
            &ch.channel_id(),
            PromotionStatus::Skipped,
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(n_skipped_after_b, 0);

        svc.handle_evaluate_push(ch.clone(), "sha-c".to_string())
            .await
            .unwrap();
        // sha-b should be Skipped now; sha-c is the new pending.
        let n_skipped_after_c = crate::db::channels::count_by_status(
            &ch.channel_id(),
            PromotionStatus::Skipped,
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(n_skipped_after_c, 1);

        // Only one Evaluating row for sha-a still exists.
        let in_flight = crate::db::channels::get_in_flight(&ch.channel_id(), &db.pool)
            .await
            .unwrap();
        assert_eq!(in_flight.unwrap().tracking_sha, "sha-a");
    }
}
