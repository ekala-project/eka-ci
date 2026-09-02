// ChannelService: orchestrates the release-channel promotion lifecycle.
//
// PR 3 lands the skeleton:
//   - the AsyncService trait impl,
//   - the idempotency guard (terminal decisions for `(channel, sha)` short-circuit re-evaluation),
//   - the coalescer wiring (writes Skipped audit rows for superseded SHAs while a previous
//     evaluation is still in flight), and
//   - a stub evaluator path: job-state snapshotting is a follow-up wired alongside RecorderService
//     -> ChannelService delivery.
//
// The intentional gaps left for PR 4 are marked with NB and isolate
// the cost of the eventual git push integration:
//   - `snapshot_job_states` returns an empty map for now, which keeps evaluation in `Waiting` until
//     the recorder pushes terminal states into this service.
//   - `perform_promotion` only logs; the actual fast-forward push lives in PR 4.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, bail};
use octocrab::Octocrab;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, instrument, warn};

use super::coalescer::{CoalesceAction, coalesce};
use super::evaluator::evaluate_promotion;
use super::types::{ChannelTask, PromotionDecision, PromotionStatus};
use crate::config::{ChannelConfig, ChannelForge};
use crate::db::DbService;
use crate::db::model::build_event::DrvBuildState;
use crate::github::GitHubTask;
use crate::services::{AsyncService, TaskJournal};

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
    /// Snapshot of the channels registry indexed by `channel_id()`.
    ///
    /// Held as `Arc` so it shares storage with the original
    /// `RuntimeConfig.channels` map without cloning per task. The
    /// registry is static for a given process lifetime (PR 1's config
    /// loader does not currently support hot-reload), so an
    /// immutable Arc is sufficient.
    channels: Arc<HashMap<String, ChannelConfig>>,
    /// GitHub API client for performing fast-forward pushes to
    /// GitHub-backed channels. `None` when GitHub integration is
    /// disabled (deployments without a GitHub App configured).
    ///
    /// GitLab and Gitea forge support will be added in a follow-up
    /// PR; today only GitHub channels can promote.
    octocrab: Option<Arc<Octocrab>>,
    /// Optional GitHub task sender for creating check runs that
    /// display promotion status in the GitHub UI. `None` when GitHub
    /// integration is disabled.
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    journal: TaskJournal<ChannelTask>,
}

impl ChannelService {
    pub fn new(
        db: DbService,
        channels: Arc<HashMap<String, ChannelConfig>>,
        octocrab: Option<Arc<Octocrab>>,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
    ) -> Self {
        let (task_sender, task_receiver) = mpsc::channel(CHANNEL_TASK_BUFFER);
        let pool = db.pool.clone();
        Self {
            task_sender,
            task_receiver: Some(task_receiver),
            db,
            pending: Arc::new(Mutex::new(HashMap::new())),
            channels,
            octocrab,
            github_sender,
            journal: TaskJournal::new(pool, "channels"),
        }
    }

    /// Fire a GitHub check run update for this channel's promotion status.
    ///
    /// Creates a check run named `release/{channel_name}` that displays
    /// the promotion state in the GitHub UI. Only fires for GitHub-backed
    /// channels when GitHub integration is enabled. Silently no-ops for
    /// other forges or when GitHub is disabled.
    fn fire_github_check_run(
        &self,
        channel: &ChannelConfig,
        sha: &str,
        status: PromotionStatus,
        blocked_reason: Option<&str>,
    ) {
        let Some(github_sender) = &self.github_sender else {
            return;
        };

        if !matches!(channel.forge, ChannelForge::GitHub) {
            return;
        }

        let task = GitHubTask::CreateChannelPromotionCheck {
            owner: channel.owner.clone(),
            repo_name: channel.repo.clone(),
            sha: sha.to_string(),
            channel_name: channel.name.clone(),
            promotion_status: status,
            blocked_reason: blocked_reason.map(String::from),
        };

        let sender = github_sender.clone();
        tokio::spawn(async move {
            if let Err(e) = sender.send(task).await {
                warn!("Failed to send CreateChannelPromotionCheck task: {:?}", e);
            }
        });
    }

    /// Snapshot the current state of every job named in
    /// `channel.required` and `channel.packages` at `sha`, indexed by
    /// the job name as declared in `.eka-ci/config.json`.
    ///
    /// Queries the DB for all jobsets matching `(owner, repo, sha)`
    /// and collects the latest `DrvBuildState` for each job name in
    /// the channel's watch list. Jobs that don't exist in any jobset
    /// are omitted, which causes the evaluator to say `Waiting`.
    #[instrument(
        skip(self),
        fields(
            channel_id = %channel.channel_id(),
            sha = %sha,
            job_count = channel.required.len() + channel.packages.len()
        )
    )]
    async fn snapshot_job_states(
        &self,
        channel: &ChannelConfig,
        sha: &str,
    ) -> Result<HashMap<String, DrvBuildState>> {
        let mut job_names = channel.required.clone();
        job_names.extend(channel.packages.clone());

        crate::db::channels::snapshot_job_states_for_sha(
            &channel.owner,
            &channel.repo,
            sha,
            &job_names,
            &self.db.pool,
        )
        .await
    }

    /// Perform a fast-forward push of `target_branch` to `tracking_sha`
    /// using the appropriate forge API.
    ///
    /// Returns `Ok(Some(previous_sha))` if the push succeeded, where
    /// `previous_sha` is the commit the target branch pointed to before
    /// the update. Returns `Ok(None)` if dry-run is enabled (no push
    /// performed). Returns `Err` if the push failed (non-fast-forward,
    /// auth failure, network error, or forge not supported).
    #[instrument(
        skip(self),
        fields(
            channel_id = %channel.channel_id(),
            sha = %sha,
            target = %channel.target_branch,
            dry_run = channel.dry_run
        )
    )]
    async fn perform_promotion(
        &self,
        channel: &ChannelConfig,
        sha: &str,
    ) -> Result<Option<String>> {
        if channel.dry_run {
            info!(
                event = "channel_promotion_dry_run",
                channel_id = %channel.channel_id(),
                sha = %sha,
                target = %channel.target_branch,
                "dry-run enabled; skipping actual push"
            );
            return Ok(None);
        }

        match channel.forge {
            ChannelForge::GitHub => self.perform_github_promotion(channel, sha).await,
            ChannelForge::GitLab { .. } | ChannelForge::Gitea { .. } => {
                bail!(
                    "channel {} uses {:?} forge; only GitHub is supported in this release",
                    channel.channel_id(),
                    channel.forge
                );
            },
        }
    }

    /// Perform a GitHub fast-forward push via the octocrab client.
    ///
    /// Uses the Git References API to update
    /// `refs/heads/{target_branch}` to point to `sha`, with
    /// `force=false` to ensure the update is a legitimate
    /// fast-forward.
    async fn perform_github_promotion(
        &self,
        channel: &ChannelConfig,
        sha: &str,
    ) -> Result<Option<String>> {
        let octocrab = self.octocrab.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "GitHub channel {} configured but octocrab client unavailable",
                channel.channel_id()
            )
        })?;

        let ref_name = format!("heads/{}", channel.target_branch);

        // Update the ref to point to the new SHA. The `force: false`
        //    parameter ensures the update is rejected if it's not a
        //    fast-forward, which prevents accidental data loss.
        //
        //    Octocrab doesn't expose a high-level `update_ref` method,
        //    so we use the underlying HTTP client directly.
        let route = format!(
            "/repos/{}/{}/git/refs/{}",
            channel.owner, channel.repo, ref_name
        );

        #[derive(serde::Serialize)]
        struct UpdateRefRequest {
            sha: String,
            force: bool,
        }

        octocrab
            ._patch(
                route,
                Some(&UpdateRefRequest {
                    sha: sha.to_string(),
                    force: false,
                }),
            )
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to fast-forward {} to {} for channel {}: {:?}",
                    channel.target_branch,
                    sha,
                    channel.channel_id(),
                    e
                )
            })?;

        info!(
            event = "channel_promoted",
            channel_id = %channel.channel_id(),
            sha = %sha,
            target = %channel.target_branch,
            "successfully fast-forwarded target branch"
        );

        // TODO: Record previous_target_sha by querying the ref before
        // updating. Requires figuring out the correct octocrab API for
        // extracting the SHA from the Ref object.
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

    #[instrument(
        skip(self),
        fields(
            channel_id = %channel.channel_id(),
            sha = %sha,
            target = %channel.target_branch
        )
    )]
    async fn handle_evaluate_push(&self, channel: ChannelConfig, sha: String) -> Result<()> {
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

                    // Update GitHub check run to neutral (skipped)
                    self.fire_github_check_run(&channel, &prev, PromotionStatus::Skipped, None);

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

        // Create GitHub check run showing evaluation in progress
        self.fire_github_check_run(&channel, &sha, PromotionStatus::Evaluating, None);

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
                let previous_target_sha = self.perform_promotion(&channel, &sha).await?;
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

                // Update GitHub check run to success
                self.fire_github_check_run(&channel, &sha, PromotionStatus::Promoted, None);

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

                // Update GitHub check run to failure with blocked reason
                self.fire_github_check_run(
                    &channel,
                    &sha,
                    PromotionStatus::Blocked,
                    Some(&blocked_json.to_string()),
                );

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

    /// React to a jobset finishing for `(forge, owner, repo)` at `sha`.
    ///
    /// The recorder cannot tell which (if any) release channels care
    /// about a given jobset, so it broadcasts the conclusion and lets
    /// ChannelService route. For each channel watching the supplied
    /// `(forge, owner, repo)`:
    ///   1. If no in-flight Evaluating row exists for the channel, the jobset completion is
    ///      irrelevant (no promotion is currently in flight that depends on it). Skip silently.
    ///   2. If the in-flight row's `tracking_sha` differs from `sha`, the jobset belongs to a
    ///      different attempt (e.g. a stale PR head, or a since-superseded SHA). Skip silently.
    ///   3. Otherwise, re-run the same evaluation pipeline as `handle_evaluate_push` *without*
    ///      re-inserting an Evaluating row (one is already open). The pipeline will either
    ///      transition the row to a terminal state or keep it Waiting until the next jobset
    ///      completion.
    #[instrument(
        skip(self),
        fields(
            forge = ?forge,
            owner = %owner,
            repo = %repo,
            sha = %sha
        )
    )]
    async fn handle_jobset_complete(
        &self,
        forge: ChannelForge,
        owner: String,
        repo: String,
        sha: String,
    ) -> Result<()> {
        // Filter channels watching this `(forge, owner, repo)`.
        // owner is case-insensitive to match the push matcher; repo
        // and forge identifier are exact.
        let matches: Vec<ChannelConfig> = self
            .channels
            .values()
            .filter(|c| c.forge == forge && c.owner.eq_ignore_ascii_case(&owner) && c.repo == repo)
            .cloned()
            .collect();

        if matches.is_empty() {
            debug!(
                event = "channel_jobset_complete_no_match",
                owner = %owner,
                repo = %repo,
                sha = %sha,
                "jobset completion has no watching channels"
            );
            return Ok(());
        }

        for channel in matches {
            let channel_id = channel.channel_id();
            let in_flight = crate::db::channels::get_in_flight(&channel_id, &self.db.pool).await?;
            let in_flight_row = match in_flight {
                Some(r) => r,
                None => {
                    debug!(
                        event = "channel_jobset_complete_no_in_flight",
                        channel_id = %channel_id,
                        sha = %sha,
                        "jobset completed but channel has no in-flight evaluation"
                    );
                    continue;
                },
            };

            if in_flight_row.tracking_sha != sha {
                debug!(
                    event = "channel_jobset_complete_sha_mismatch",
                    channel_id = %channel_id,
                    jobset_sha = %sha,
                    in_flight_sha = %in_flight_row.tracking_sha,
                    "jobset SHA differs from in-flight evaluation; ignoring"
                );
                continue;
            }

            // Re-snapshot job states and re-evaluate. The Evaluating
            // row is already open; finalise_evaluation flips it to a
            // terminal status if the decision is Ready or Blocked,
            // and otherwise we stay Waiting for the next conclusion.
            let job_states = self.snapshot_job_states(&channel, &sha).await?;
            let decision = evaluate_promotion(&channel, &job_states);

            match decision {
                PromotionDecision::Ready { required_results } => {
                    let previous_target_sha = self.perform_promotion(&channel, &sha).await?;
                    let required_json = serde_json::to_string(&required_results)
                        .unwrap_or_else(|_| "{}".to_string());
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

                    // Update GitHub check run to success
                    self.fire_github_check_run(&channel, &sha, PromotionStatus::Promoted, None);

                    info!(
                        event = "channel_promoted",
                        channel_id = %channel_id,
                        sha = %sha,
                        trigger = "jobset_complete",
                        "channel evaluation reached Promoted (PR3 stub: no actual push)"
                    );
                    // After a terminal decision, promote the pending
                    // SHA (if any) to a fresh evaluation.
                    self.drain_pending(&channel).await?;
                },
                PromotionDecision::Blocked {
                    failed_required,
                    required_results,
                } => {
                    let blocked_json = serde_json::json!({
                        "failed_required": failed_required,
                    });
                    let required_json = serde_json::to_string(&required_results)
                        .unwrap_or_else(|_| "{}".to_string());
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

                    // Update GitHub check run to failure with blocked reason
                    self.fire_github_check_run(
                        &channel,
                        &sha,
                        PromotionStatus::Blocked,
                        Some(&blocked_json.to_string()),
                    );

                    warn!(
                        event = "channel_blocked",
                        channel_id = %channel_id,
                        sha = %sha,
                        failed = ?failed_required,
                        trigger = "jobset_complete",
                        "channel evaluation blocked by failed required jobs"
                    );
                    self.drain_pending(&channel).await?;
                },
                PromotionDecision::Waiting {
                    pending_required,
                    pending_packages,
                } => {
                    debug!(
                        event = "channel_evaluation_waiting",
                        channel_id = %channel_id,
                        sha = %sha,
                        pending_required = ?pending_required,
                        pending_packages = ?pending_packages,
                        trigger = "jobset_complete",
                        "channel still waiting on additional job states"
                    );
                },
            }
        }

        Ok(())
    }

    /// After a terminal decision, kick off evaluation of any queued
    /// pending SHA. The pending entry is removed and a fresh
    /// `EvaluatePush` task is dispatched through `self.task_sender`
    /// so the work flows back through the normal coalescer path,
    /// including the idempotency guard.
    #[instrument(
        skip(self),
        fields(channel_id = %channel.channel_id())
    )]
    async fn drain_pending(&self, channel: &ChannelConfig) -> Result<()> {
        let channel_id = channel.channel_id();
        let pending_sha = {
            let mut pending = self.pending.lock().await;
            pending.remove(&channel_id)
        };
        if let Some(sha) = pending_sha {
            debug!(
                event = "channel_drain_pending",
                channel_id = %channel_id,
                pending_sha = %sha,
                "in-flight evaluation finished; promoting pending SHA"
            );
            if let Err(e) = self
                .task_sender
                .send(ChannelTask::EvaluatePush {
                    channel: channel.clone(),
                    sha,
                })
                .await
            {
                warn!(
                    event = "channel_drain_pending_send_failed",
                    channel_id = %channel_id,
                    error = ?e,
                    "failed to enqueue pending SHA evaluation"
                );
            }
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

    fn task_journal(&self) -> Option<&TaskJournal<ChannelTask>> {
        Some(&self.journal)
    }

    async fn handle_task(&self, task: ChannelTask) -> Result<()> {
        match task {
            ChannelTask::EvaluatePush { channel, sha } => {
                self.handle_evaluate_push(channel, sha).await
            },
            ChannelTask::JobsetComplete {
                forge,
                owner,
                repo,
                sha,
            } => self.handle_jobset_complete(forge, owner, repo, sha).await,
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
            dry_run: true, // Tests use dry-run mode to avoid needing octocrab
        }
    }

    #[tokio::test]
    async fn fresh_push_with_empty_required_promotes_immediately() {
        // With required=[] and packages=[] the pure evaluator returns
        // Ready; the service should record a Promoted row. (PR 3
        // stub: no actual FF push.)
        let db = DbService::new_in_memory().await.unwrap();
        let svc = ChannelService::new(db.clone(), Arc::new(HashMap::new()), None, None);
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
        let svc = ChannelService::new(db.clone(), Arc::new(HashMap::new()), None, None);
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
        let svc = ChannelService::new(db.clone(), Arc::new(HashMap::new()), None, None);
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
        let svc = ChannelService::new(db.clone(), Arc::new(HashMap::new()), None, None);
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

    #[tokio::test]
    async fn jobset_complete_for_unknown_repo_is_noop() {
        // Empty channels registry: a JobsetComplete for any repo
        // should silently no-op without touching the DB.
        let db = DbService::new_in_memory().await.unwrap();
        let svc = ChannelService::new(db.clone(), Arc::new(HashMap::new()), None, None);
        svc.handle_jobset_complete(
            ChannelForge::GitHub,
            "no-such-owner".to_string(),
            "no-such-repo".to_string(),
            "sha-x".to_string(),
        )
        .await
        .unwrap();
        // No promotions of any kind should have been written.
        let n_eval = crate::db::channels::count_by_status(
            "github:no-such-owner/no-such-repo:none",
            PromotionStatus::Evaluating,
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(n_eval, 0);
    }

    #[tokio::test]
    async fn jobset_complete_with_no_in_flight_is_noop() {
        // Channel exists in registry but has no in-flight Evaluating
        // row; the JobsetComplete should not synthesize one.
        let db = DbService::new_in_memory().await.unwrap();
        let ch = channel("stable", &["coreutils"], &[]);
        let mut registry = HashMap::new();
        registry.insert(ch.channel_id(), ch.clone());
        let svc = ChannelService::new(db.clone(), Arc::new(registry), None, None);

        svc.handle_jobset_complete(
            ChannelForge::GitHub,
            ch.owner.clone(),
            ch.repo.clone(),
            "sha-without-evaluation".to_string(),
        )
        .await
        .unwrap();

        let n_eval = crate::db::channels::count_by_status(
            &ch.channel_id(),
            PromotionStatus::Evaluating,
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(n_eval, 0);
        let n_promoted = crate::db::channels::count_by_status(
            &ch.channel_id(),
            PromotionStatus::Promoted,
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(n_promoted, 0);
    }

    #[tokio::test]
    async fn jobset_complete_for_different_sha_is_noop() {
        // sha-a is Evaluating (Waiting on coreutils); a jobset for
        // an unrelated sha-b completes. The Evaluating row must not
        // be touched.
        let db = DbService::new_in_memory().await.unwrap();
        let ch = channel("stable", &["coreutils"], &[]);
        let mut registry = HashMap::new();
        registry.insert(ch.channel_id(), ch.clone());
        let svc = ChannelService::new(db.clone(), Arc::new(registry), None, None);

        // Open an Evaluating row for sha-a.
        svc.handle_evaluate_push(ch.clone(), "sha-a".to_string())
            .await
            .unwrap();
        // The stub snapshot returns empty, so this stayed Waiting.

        svc.handle_jobset_complete(
            ChannelForge::GitHub,
            ch.owner.clone(),
            ch.repo.clone(),
            "sha-b".to_string(),
        )
        .await
        .unwrap();

        let in_flight = crate::db::channels::get_in_flight(&ch.channel_id(), &db.pool)
            .await
            .unwrap()
            .expect("Evaluating row for sha-a should still exist");
        assert_eq!(in_flight.tracking_sha, "sha-a");
    }

    #[tokio::test]
    async fn jobset_complete_for_in_flight_sha_keeps_waiting_when_snapshot_empty() {
        // The stub `snapshot_job_states` returns an empty map, so
        // even after a JobsetComplete the evaluator still says
        // Waiting. The Evaluating row must remain in place — this
        // verifies the dispatch wires through but no spurious
        // terminal transition occurs in the PR 3 stub configuration.
        let db = DbService::new_in_memory().await.unwrap();
        let ch = channel("stable", &["coreutils"], &[]);
        let mut registry = HashMap::new();
        registry.insert(ch.channel_id(), ch.clone());
        let svc = ChannelService::new(db.clone(), Arc::new(registry), None, None);

        svc.handle_evaluate_push(ch.clone(), "sha-a".to_string())
            .await
            .unwrap();

        svc.handle_jobset_complete(
            ChannelForge::GitHub,
            ch.owner.clone(),
            ch.repo.clone(),
            "sha-a".to_string(),
        )
        .await
        .unwrap();

        let in_flight = crate::db::channels::get_in_flight(&ch.channel_id(), &db.pool)
            .await
            .unwrap();
        assert!(
            in_flight.is_some(),
            "Evaluating row should persist while snapshot stub returns empty map"
        );
    }

    #[tokio::test]
    async fn jobset_complete_drives_promotion_when_no_required_jobs() {
        // A channel with empty required+packages lists already
        // reaches Ready on the initial EvaluatePush; the Promoted
        // row is written there. JobsetComplete arriving afterwards
        // for that same SHA must be idempotent — no in-flight row
        // remains, so the dispatch silently no-ops.
        let db = DbService::new_in_memory().await.unwrap();
        let ch = channel("stable", &[], &[]);
        let mut registry = HashMap::new();
        registry.insert(ch.channel_id(), ch.clone());
        let svc = ChannelService::new(db.clone(), Arc::new(registry), None, None);

        svc.handle_evaluate_push(ch.clone(), "sha-x".to_string())
            .await
            .unwrap();

        // Verify we are in the expected post-condition: one Promoted
        // row, no in-flight row.
        let n_promoted = crate::db::channels::count_by_status(
            &ch.channel_id(),
            PromotionStatus::Promoted,
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(n_promoted, 1);
        assert!(
            crate::db::channels::get_in_flight(&ch.channel_id(), &db.pool)
                .await
                .unwrap()
                .is_none()
        );

        // Late-arriving JobsetComplete: should not write a second
        // Promoted row.
        svc.handle_jobset_complete(
            ChannelForge::GitHub,
            ch.owner.clone(),
            ch.repo.clone(),
            "sha-x".to_string(),
        )
        .await
        .unwrap();

        let n_promoted_after = crate::db::channels::count_by_status(
            &ch.channel_id(),
            PromotionStatus::Promoted,
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(
            n_promoted_after, 1,
            "JobsetComplete after terminal decision must be idempotent"
        );
    }
}
