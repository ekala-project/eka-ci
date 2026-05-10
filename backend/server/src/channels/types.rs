// Shared types for the release-channel subsystem.
//
// Kept as a small, plain-data module so the pure evaluator/coalescer
// modules don't need to depend on the heavier service module — and so
// unit tests can construct decisions and tasks without spinning up the
// async runtime.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::{ChannelConfig, ChannelForge};
use crate::db::model::build_event::DrvBuildState;

/// Tasks accepted by the ChannelService over its mpsc channel.
///
/// Today PR 3 only exposes a single variant; later PRs will add
/// variants for "job set completed" (delivered by the recorder), CLI
/// triggers, and an admin "force re-evaluate" path.
#[allow(dead_code)] // variants are constructed by producers in PR 4
#[derive(Debug, Clone)]
pub enum ChannelTask {
    /// A push to the channel's tracking-branch was observed at `sha`.
    ///
    /// The ChannelService will:
    ///   1. Refuse the task if `(channel_id, sha)` already has a
    ///      terminal decision recorded (idempotency).
    ///   2. Run the coalescer: if a different sha is already
    ///      Evaluating, mark the new attempt as Skipped immediately
    ///      and bail; if no in-flight row exists, open an Evaluating
    ///      row for this sha.
    ///   3. Snapshot the current job-states and run the pure
    ///      evaluator. (Wired in a follow-up PR once a `JobsetComplete`
    ///      event from the recorder drives re-evaluation.)
    EvaluatePush {
        channel: ChannelConfig,
        sha: String,
    },

    /// A jobset run at `sha` for `(forge, owner, repo)` has concluded
    /// (every Job in the jobset reached a terminal `DrvBuildState`).
    ///
    /// Emitted by the RecorderService when it detects the final
    /// `is_terminal()` transition that flips
    /// `DbService::all_jobs_concluded` to true. The recorder fires it
    /// generically: ChannelService is responsible for filtering down
    /// to channels actually watching that `(forge, owner, repo)` and,
    /// among those, only the ones with an in-flight Evaluating row
    /// whose `tracking_sha == sha`. All other arrivals are no-ops.
    JobsetComplete {
        forge: ChannelForge,
        owner: String,
        repo: String,
        sha: String,
    },
}

/// Numeric status as written to the `ChannelPromotion.status` column.
///
/// The discriminants are part of the storage contract — never reorder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum PromotionStatus {
    /// In-flight: still waiting for required / packages jobs.
    Evaluating = 0,
    /// At least one `required` job entered a failure terminal state.
    Blocked = 1,
    /// `target_branch` was fast-forwarded successfully (or, in
    /// `dry_run` mode, would have been).
    Promoted = 2,
    /// The fast-forward push to the forge failed (non-FF, auth, etc.).
    PushFailed = 3,
    /// The coalescer observed this SHA after a newer one had already
    /// pre-empted it; recorded for the audit trail but no work done.
    Skipped = 4,
}

impl PromotionStatus {
    pub fn as_i64(self) -> i64 {
        self as i64
    }
}

/// Pure decision returned by [`crate::channels::evaluator::evaluate_promotion`].
///
/// The variants line up 1:1 with the PromotionStatus terminal values
/// the service will subsequently record, EXCEPT for `Waiting` which
/// keeps the row in `Evaluating` until more job states arrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromotionDecision {
    /// All required jobs succeeded and all package jobs reached a
    /// terminal state. The service may proceed to the FF push step.
    Ready {
        /// `{required_job_name -> terminal state}` for posterity.
        required_results: BTreeMap<String, DrvBuildState>,
    },
    /// At least one `required` job is in a failure terminal state.
    /// `failed_required` lists the offenders (sorted) so the
    /// blocked_reason JSON is deterministic across runs.
    Blocked {
        failed_required: Vec<String>,
        required_results: BTreeMap<String, DrvBuildState>,
    },
    /// Not enough information yet. Lists the job names whose state is
    /// either missing or non-terminal so operators can see at a glance
    /// what we're waiting on.
    Waiting {
        pending_required: Vec<String>,
        pending_packages: Vec<String>,
    },
}

/// Serializable summary of a promotion decision, suitable for writing
/// into the `blocked_reason` JSON column.
///
/// Kept as a small `Serialize`/`Deserialize` type so the storage shape
/// can evolve independently from `PromotionDecision`'s pattern-matching
/// API. The CLI status subcommand (PR 5) will read these back.
#[allow(dead_code)] // reader lands with the CLI subcommand in PR 5
#[derive(Debug, Serialize, Deserialize)]
pub struct BlockedReason {
    /// `required` job names that observed a failure terminal state.
    pub failed_required: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promotion_status_discriminants_are_stable() {
        // These are persisted in SQLite; reordering breaks history.
        assert_eq!(PromotionStatus::Evaluating.as_i64(), 0);
        assert_eq!(PromotionStatus::Blocked.as_i64(), 1);
        assert_eq!(PromotionStatus::Promoted.as_i64(), 2);
        assert_eq!(PromotionStatus::PushFailed.as_i64(), 3);
        assert_eq!(PromotionStatus::Skipped.as_i64(), 4);
    }
}
