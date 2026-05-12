// Pure coalescer for release-channel promotion attempts.
//
// Background
// ----------
// A push to a channel's tracking-branch enqueues an `EvaluatePush`
// task. Pushes can arrive faster than the eval+build pipeline can
// validate them — e.g. three commits land within a minute while a
// 30-minute evaluation of the first commit is still in flight. The
// `ChannelPromotionInFlight` partial UNIQUE index in the DB schema
// permits at most one Evaluating row per channel, so the service has
// to decide deterministically what to do with each subsequent SHA.
//
// Policy
// ------
// Latest-wins: keep the in-flight evaluation running (don't abort it,
// the work is already paid for) and remember the *newest* candidate
// SHA as the "pending" one. Any earlier candidate that arrived while
// the in-flight row was busy is recorded as `Skipped` for audit so
// operators can reconstruct what eka-ci chose to do and why.
//
// This module is pure so the policy is exhaustively testable without
// the database.

/// What the ChannelService should do with an incoming candidate SHA.
///
/// The transitions are intentionally restricted: there is no
/// "AbortInFlight" — pre-empting an already-running evaluation would
/// waste expensive build work and create races with the recorder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoalesceAction {
    /// No in-flight row exists; open a fresh `Evaluating` row for the
    /// candidate SHA and proceed to drive the evaluator.
    StartFresh,

    /// An in-flight row already exists for the *same* SHA. This is
    /// the duplicate-webhook case (forges sometimes redeliver). The
    /// service should silently no-op so the audit log doesn't fill
    /// with spurious Skipped rows.
    AlreadyInFlight,

    /// An in-flight row exists for a *different* SHA. The candidate
    /// becomes the channel's new pending-SHA (kept in memory by the
    /// service); if a previous pending-SHA existed it is now
    /// superseded and the service should record a `Skipped` row for
    /// it. `previous_pending_sha` carries that bookkeeping outwards
    /// so the I/O lives in the service, not the pure function.
    DeferAsPending {
        previous_pending_sha: Option<String>,
    },
}

/// Decide what to do with `candidate_sha`, given the channel's current
/// in-flight SHA (`Evaluating` row in the DB) and the previously
/// remembered pending SHA (in-memory in the service).
///
/// Inputs (all pure):
///
/// * `in_flight_sha`: `Some(sha)` if there is an `Evaluating` ChannelPromotion row for this
///   channel, else `None`.
/// * `current_pending_sha`: `Some(sha)` if the service had already queued a newer SHA waiting for
///   `in_flight_sha` to complete.
/// * `candidate_sha`: the SHA carried by the just-arrived `EvaluatePush` task.
pub fn coalesce(
    in_flight_sha: Option<&str>,
    current_pending_sha: Option<&str>,
    candidate_sha: &str,
) -> CoalesceAction {
    match in_flight_sha {
        None => CoalesceAction::StartFresh,
        Some(sha) if sha == candidate_sha => CoalesceAction::AlreadyInFlight,
        Some(_) => {
            // An older evaluation is still running. If the candidate
            // matches the SHA already queued as pending, treat it as
            // a duplicate (no audit row needed). Otherwise the new
            // candidate supersedes whatever was previously pending.
            match current_pending_sha {
                Some(prev) if prev == candidate_sha => CoalesceAction::AlreadyInFlight,
                Some(prev) => CoalesceAction::DeferAsPending {
                    previous_pending_sha: Some(prev.to_string()),
                },
                None => CoalesceAction::DeferAsPending {
                    previous_pending_sha: None,
                },
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::types::PromotionStatus;

    #[test]
    fn start_fresh_when_no_in_flight() {
        assert_eq!(coalesce(None, None, "sha-a"), CoalesceAction::StartFresh);
    }

    #[test]
    fn start_fresh_when_no_in_flight_and_pending_present_is_impossible_but_safe() {
        // The service should never carry a pending SHA when there is
        // no in-flight row (it would have promoted the pending one
        // immediately). The pure function nonetheless prefers
        // StartFresh: the in-memory pending state must have been
        // stale, and starting fresh is harmless because the
        // ChannelService re-checks idempotency afterwards.
        assert_eq!(
            coalesce(None, Some("sha-pending"), "sha-new"),
            CoalesceAction::StartFresh
        );
    }

    #[test]
    fn redelivery_of_in_flight_sha_is_noop() {
        assert_eq!(
            coalesce(Some("sha-a"), None, "sha-a"),
            CoalesceAction::AlreadyInFlight
        );
    }

    #[test]
    fn new_sha_while_in_flight_becomes_pending() {
        // First newer commit arrives during evaluation -> defer with
        // no previous pending.
        assert_eq!(
            coalesce(Some("sha-a"), None, "sha-b"),
            CoalesceAction::DeferAsPending {
                previous_pending_sha: None
            }
        );
    }

    #[test]
    fn newer_sha_supersedes_previous_pending() {
        // sha-a is evaluating, sha-b had been pending; sha-c arrives.
        // sha-b is now obsolete and must be audited as Skipped.
        assert_eq!(
            coalesce(Some("sha-a"), Some("sha-b"), "sha-c"),
            CoalesceAction::DeferAsPending {
                previous_pending_sha: Some("sha-b".to_string())
            }
        );
    }

    #[test]
    fn redelivery_of_pending_sha_is_noop() {
        // Forges occasionally redeliver. If sha-b is already pending
        // and the same sha-b shows up again, no audit row is
        // warranted.
        assert_eq!(
            coalesce(Some("sha-a"), Some("sha-b"), "sha-b"),
            CoalesceAction::AlreadyInFlight
        );
    }

    #[test]
    fn skipped_status_discriminant_is_four() {
        // Sanity: the DB schema reserves status=4 for Skipped rows.
        // The coalescer's `DeferAsPending` carries the superseded
        // SHA that the service writes with this status, so a silent
        // renumbering would corrupt audit history.
        assert_eq!(PromotionStatus::Skipped.as_i64(), 4);
    }
}
