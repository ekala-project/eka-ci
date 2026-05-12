// Pure promotion evaluator.
//
// Decides whether a release-channel may advance, given:
//   - the channel's `required` and `packages` job-name lists, and
//   - a snapshot of `{job_name -> latest DrvBuildState}` collected at the channel's tracking-sha.
//
// This module is intentionally side-effect-free so unit tests can
// drive every interesting state combination without touching the
// database or any async machinery.

use std::collections::{BTreeMap, HashMap};

use super::types::PromotionDecision;
use crate::config::ChannelConfig;
use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};

/// Pure evaluator: derive a [`PromotionDecision`] from the channel's
/// configured job lists and an observed `job_states` snapshot.
///
/// Semantics:
///
/// * A `required` job blocks promotion until it reaches `Completed(Success)`. Any failure terminal
///   state (Failure, TransitiveFailure, Interrupted(_), UnsatisfiableRequirements) yields `Blocked`
///   — *every* offender is listed so the audit row names them all rather than just the first one
///   observed.
/// * A `packages` job blocks promotion only until it reaches *any* terminal state; its individual
///   success/failure outcome doesn't gate promotion.
/// * A missing entry in `job_states` is treated as non-terminal: we don't yet have evidence the job
///   has finished, so we wait.
///
/// `Blocked` takes precedence over `Waiting` even if some packages are
/// still pending: once a required job has failed, no amount of waiting
/// will rescue the promotion.
pub fn evaluate_promotion(
    channel: &ChannelConfig,
    job_states: &HashMap<String, DrvBuildState>,
) -> PromotionDecision {
    // First pass: collect the state of every `required` job. We need
    // this for both the Blocked-decision required_results JSON and to
    // detect failures.
    let mut required_results: BTreeMap<String, DrvBuildState> = BTreeMap::new();
    let mut failed_required: Vec<String> = Vec::new();
    let mut pending_required: Vec<String> = Vec::new();

    for name in &channel.required {
        match job_states.get(name) {
            Some(state) if state.is_failure() => {
                failed_required.push(name.clone());
                required_results.insert(name.clone(), state.clone());
            },
            Some(state) if matches!(state, DrvBuildState::Completed(DrvBuildResult::Success)) => {
                required_results.insert(name.clone(), state.clone());
            },
            // Terminal-but-not-success that wasn't caught above is
            // theoretically unreachable today (is_failure covers all
            // non-Success terminal cases), but guard anyway so future
            // states default to "blocked": a terminal non-success
            // required job MUST NOT promote.
            Some(state) if state.is_terminal() => {
                failed_required.push(name.clone());
                required_results.insert(name.clone(), state.clone());
            },
            Some(_) | None => {
                pending_required.push(name.clone());
            },
        }
    }

    if !failed_required.is_empty() {
        // Sort for deterministic JSON output. The DB column is
        // searched and diffed by operators, so a stable order matters.
        failed_required.sort();
        return PromotionDecision::Blocked {
            failed_required,
            required_results,
        };
    }

    // Required all succeeded. Now check `packages` for terminal state.
    let mut pending_packages: Vec<String> = Vec::new();
    for name in &channel.packages {
        match job_states.get(name) {
            Some(state) if state.is_terminal() => {
                // Terminal regardless of outcome; doesn't gate.
            },
            Some(_) | None => {
                pending_packages.push(name.clone());
            },
        }
    }

    if pending_required.is_empty() && pending_packages.is_empty() {
        return PromotionDecision::Ready { required_results };
    }

    pending_required.sort();
    pending_packages.sort();
    PromotionDecision::Waiting {
        pending_required,
        pending_packages,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::config::{ChannelConfig, ChannelForge};
    use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};

    fn channel(required: &[&str], packages: &[&str]) -> ChannelConfig {
        ChannelConfig {
            forge: ChannelForge::GitHub,
            owner: "ekacorp".to_string(),
            repo: "ekapkgs".to_string(),
            name: "stable".to_string(),
            tracking_branch: "master".to_string(),
            target_branch: "ekapkgs-unstable".to_string(),
            required: required.iter().map(|s| s.to_string()).collect(),
            packages: packages.iter().map(|s| s.to_string()).collect(),
            dry_run: false,
        }
    }

    fn states(items: &[(&str, DrvBuildState)]) -> HashMap<String, DrvBuildState> {
        items
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn success() -> DrvBuildState {
        DrvBuildState::Completed(DrvBuildResult::Success)
    }
    fn failure() -> DrvBuildState {
        DrvBuildState::Completed(DrvBuildResult::Failure)
    }

    #[test]
    fn ready_when_all_required_succeed_and_packages_terminal() {
        let ch = channel(&["coreutils"], &["coreutils", "hello"]);
        let s = states(&[
            ("coreutils", success()),
            ("hello", DrvBuildState::TransitiveFailure),
        ]);
        let decision = evaluate_promotion(&ch, &s);
        match decision {
            PromotionDecision::Ready { required_results } => {
                assert_eq!(required_results.len(), 1);
                assert_eq!(required_results.get("coreutils"), Some(&success()));
            },
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn ready_when_required_empty_and_packages_empty() {
        // Defensive: a channel with no jobs declared is trivially Ready.
        // Admin validation should prevent this in practice, but the
        // pure function must remain well-defined.
        let ch = channel(&[], &[]);
        let s = HashMap::new();
        let decision = evaluate_promotion(&ch, &s);
        assert!(matches!(decision, PromotionDecision::Ready { .. }));
    }

    #[test]
    fn blocked_when_required_failed() {
        let ch = channel(&["coreutils"], &["coreutils"]);
        let s = states(&[("coreutils", failure())]);
        match evaluate_promotion(&ch, &s) {
            PromotionDecision::Blocked {
                failed_required,
                required_results,
            } => {
                assert_eq!(failed_required, vec!["coreutils".to_string()]);
                assert_eq!(required_results.get("coreutils"), Some(&failure()));
            },
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn blocked_when_required_transitive_failure() {
        let ch = channel(&["coreutils"], &[]);
        let s = states(&[("coreutils", DrvBuildState::TransitiveFailure)]);
        assert!(matches!(
            evaluate_promotion(&ch, &s),
            PromotionDecision::Blocked { .. }
        ));
    }

    #[test]
    fn blocked_when_required_interrupted() {
        let ch = channel(&["coreutils"], &[]);
        let s = states(&[(
            "coreutils",
            DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout),
        )]);
        assert!(matches!(
            evaluate_promotion(&ch, &s),
            PromotionDecision::Blocked { .. }
        ));
    }

    #[test]
    fn blocked_when_required_unsatisfiable() {
        let ch = channel(&["coreutils"], &[]);
        let s = states(&[("coreutils", DrvBuildState::UnsatisfiableRequirements)]);
        assert!(matches!(
            evaluate_promotion(&ch, &s),
            PromotionDecision::Blocked { .. }
        ));
    }

    #[test]
    fn blocked_lists_every_offender_sorted() {
        // The audit row should name *all* failing required jobs, so a
        // human triaging a failed promotion doesn't have to fix one
        // job, retry, and then discover another.
        let ch = channel(&["zzz-pkg", "aaa-pkg", "mid-pkg"], &[]);
        let s = states(&[
            ("zzz-pkg", failure()),
            ("aaa-pkg", DrvBuildState::TransitiveFailure),
            ("mid-pkg", success()),
        ]);
        match evaluate_promotion(&ch, &s) {
            PromotionDecision::Blocked {
                failed_required, ..
            } => {
                assert_eq!(
                    failed_required,
                    vec!["aaa-pkg".to_string(), "zzz-pkg".to_string()]
                );
            },
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn blocked_takes_precedence_over_waiting() {
        // One required job has already failed, even though another
        // required job and some packages are still pending. The
        // operator should be told "blocked" so they fix the failure
        // rather than keep waiting.
        let ch = channel(&["good", "bad"], &["slow-pkg"]);
        let s = states(&[("bad", failure()), ("good", DrvBuildState::Building)]);
        match evaluate_promotion(&ch, &s) {
            PromotionDecision::Blocked {
                failed_required, ..
            } => {
                assert_eq!(failed_required, vec!["bad".to_string()]);
            },
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn waiting_when_required_not_terminal() {
        let ch = channel(&["coreutils"], &[]);
        let s = states(&[("coreutils", DrvBuildState::Building)]);
        match evaluate_promotion(&ch, &s) {
            PromotionDecision::Waiting {
                pending_required,
                pending_packages,
            } => {
                assert_eq!(pending_required, vec!["coreutils".to_string()]);
                assert!(pending_packages.is_empty());
            },
            other => panic!("expected Waiting, got {other:?}"),
        }
    }

    #[test]
    fn waiting_when_required_missing_from_snapshot() {
        // Absent from the snapshot = we don't yet know -> Waiting,
        // not Ready. This is the early-pipeline case: the recorder
        // hasn't seen the job's first state event yet.
        let ch = channel(&["coreutils"], &[]);
        let s = HashMap::new();
        match evaluate_promotion(&ch, &s) {
            PromotionDecision::Waiting {
                pending_required, ..
            } => {
                assert_eq!(pending_required, vec!["coreutils".to_string()]);
            },
            other => panic!("expected Waiting, got {other:?}"),
        }
    }

    #[test]
    fn waiting_when_packages_not_terminal() {
        let ch = channel(&["coreutils"], &["coreutils", "slow"]);
        let s = states(&[("coreutils", success()), ("slow", DrvBuildState::Queued)]);
        match evaluate_promotion(&ch, &s) {
            PromotionDecision::Waiting {
                pending_required,
                pending_packages,
            } => {
                assert!(pending_required.is_empty());
                assert_eq!(pending_packages, vec!["slow".to_string()]);
            },
            other => panic!("expected Waiting, got {other:?}"),
        }
    }

    #[test]
    fn packages_failure_does_not_block() {
        // The point of `packages` (vs `required`) is precisely that
        // a non-required package failure does NOT block promotion —
        // it just has to have been *attempted*.
        let ch = channel(&["coreutils"], &["coreutils", "broken"]);
        let s = states(&[("coreutils", success()), ("broken", failure())]);
        assert!(matches!(
            evaluate_promotion(&ch, &s),
            PromotionDecision::Ready { .. }
        ));
    }

    #[test]
    fn pending_lists_are_sorted() {
        let ch = channel(&["z", "a", "m"], &["yy", "aa", "mm"]);
        let s = states(&[("a", success())]); // m, z still pending; all packages pending
        match evaluate_promotion(&ch, &s) {
            PromotionDecision::Waiting {
                pending_required,
                pending_packages,
            } => {
                assert_eq!(pending_required, vec!["m".to_string(), "z".to_string()]);
                assert_eq!(
                    pending_packages,
                    vec!["aa".to_string(), "mm".to_string(), "yy".to_string()]
                );
            },
            other => panic!("expected Waiting, got {other:?}"),
        }
    }
}
