//! Periodic reconciliation of open PRs and merge-queue groups against
//! evaluations.
//!
//! Webhooks and in-memory message passing are not enough to guarantee a
//! commit gets evaluated: a webhook can arrive while the server is down,
//! and tasks waiting in a service's channel are lost on restart or behind
//! a stuck service. The reconciler is the level-triggered backstop: every
//! [`RECONCILE_INTERVAL`], for each installed repository, it lists the
//! open PRs ([`pull_requests`]) and live merge-queue groups
//! ([`merge_queue`]) on GitHub, and queues an evaluation for any head
//! commit that has no jobset and no evaluation in flight.
//!
//! Work is identified by head commit SHA: a new head (push, rebase, new
//! queue group) is a new commit to evaluate, and a head that was already
//! evaluated is left alone (the pipeline's `needs_eval` skips existing
//! jobsets anyway).

mod merge_queue;
mod pull_requests;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use octocrab::Octocrab;
use octocrab::models::InstallationId;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::config::GitHubAppConfig;
use crate::db::DbService;
use crate::db::github::{EvalRequest, HeadCommit};
use crate::git::GitTask;

/// How often open PRs and merge groups are reconciled.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Delay before the first pass, letting journal replay finish first.
const STARTUP_DELAY: Duration = Duration::from_secs(60);
/// A request queued by this process is presumed in flight for this long
/// (comfortably above the default eval timeout) before it is retried.
const STALE_AFTER_SECS: i64 = 2 * 60 * 60;
/// Evaluations the reconciler queues for one head commit before giving up.
const MAX_RECONCILE_ATTEMPTS: i64 = 3;

/// What to do with a head commit.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Evaluated (jobset exists, or finished without one).
    Done,
    /// Queued recently by this process; still running.
    InFlight,
    /// The reconciler already retried it too often.
    Exhausted,
    /// Needs (re-)queuing.
    Enqueue,
}

/// Decide whether a head commit needs an evaluation queued.
///
/// Requests from before `process_start` are never in flight: whatever
/// was queued in memory was lost when that process exited.
fn decide(
    has_jobset: bool,
    request: Option<&EvalRequest>,
    now: i64,
    process_start: i64,
) -> Decision {
    if has_jobset {
        return Decision::Done;
    }
    let Some(request) = request else {
        return Decision::Enqueue;
    };
    if request.completed_at.is_some() {
        return Decision::Done;
    }
    if request.attempts >= MAX_RECONCILE_ATTEMPTS {
        return Decision::Exhausted;
    }
    let queued_by_this_process = request.requested_at >= process_start;
    if queued_by_this_process && now - request.requested_at < STALE_AFTER_SECS {
        return Decision::InFlight;
    }
    Decision::Enqueue
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Record that a PR or merge-group head finished evaluating without
/// producing a jobset (no jobs configured, or its eval failed), so the
/// reconciler doesn't re-queue it. No-op for branch commits. Failures are
/// only logged: the worst case is one redundant re-evaluation.
pub async fn mark_head_evaluated(ci_info: &super::CICheckInfo, db_service: &DbService) {
    if ci_info.base_commit.is_none() {
        return;
    }
    let head = HeadCommit {
        owner: &ci_info.owner,
        repo_name: &ci_info.repo_name,
        sha: &ci_info.commit,
    };
    if let Err(e) = crate::db::github::mark_eval_request_completed(head, &db_service.pool).await {
        warn!(
            "Failed to mark head {} of {}/{} evaluated: {:?}",
            head.sha, head.owner, head.repo_name, e
        );
    }
}

/// Approval requirements, mirroring the webhook handlers.
#[derive(Debug, Clone, Copy)]
pub struct ApprovalPolicy {
    /// `require_approval`: PR authors must be approved.
    pub pull_requests: bool,
    /// `merge_queue_require_approval`: merge-group authors must be approved.
    pub merge_queue: bool,
}

pub struct EvalReconciler {
    octocrab: Octocrab,
    db_service: DbService,
    git_sender: mpsc::Sender<GitTask>,
    approval: ApprovalPolicy,
    github_app_configs: Arc<HashMap<String, GitHubAppConfig>>,
    process_start: i64,
}

impl EvalReconciler {
    pub fn new(
        octocrab: Octocrab,
        db_service: DbService,
        git_sender: mpsc::Sender<GitTask>,
        approval: ApprovalPolicy,
        github_app_configs: Arc<HashMap<String, GitHubAppConfig>>,
    ) -> Self {
        Self {
            octocrab,
            db_service,
            git_sender,
            approval,
            github_app_configs,
            process_start: unix_now(),
        }
    }

    pub fn run(self, cancel_token: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            let start = tokio::time::Instant::now() + STARTUP_DELAY;
            let mut interval = tokio::time::interval_at(start, RECONCILE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = cancel_token.cancelled() => break,
                    _ = interval.tick() => {
                        if let Err(e) = self.reconcile_all().await {
                            warn!("Eval reconciliation failed: {:?}", e);
                        }
                    }
                }
            }
            debug!("Eval reconciler shutting down");
        })
    }

    async fn reconcile_all(&self) -> Result<()> {
        let repos = crate::db::github::list_repositories(&self.db_service.pool).await?;
        for repo in repos {
            let octocrab = match self
                .octocrab
                .installation(InstallationId(repo.installation_id as u64))
            {
                Ok(octocrab) => octocrab,
                Err(e) => {
                    warn!(
                        "No installation client for {}/{}: {:?}",
                        repo.owner, repo.repo_name, e
                    );
                    continue;
                },
            };
            if let Err(e) = self.reconcile_open_prs(&octocrab, &repo).await {
                warn!(
                    "Failed to reconcile PRs of {}/{}: {:?}",
                    repo.owner, repo.repo_name, e
                );
            }
            if let Err(e) = self.reconcile_merge_groups(&octocrab, &repo).await {
                warn!(
                    "Failed to reconcile merge queue of {}/{}: {:?}",
                    repo.owner, repo.repo_name, e
                );
            }
        }
        Ok(())
    }

    /// Decide what to do with `head`; also returns its request row.
    async fn head_decision(&self, head: HeadCommit<'_>) -> Result<(Decision, Option<EvalRequest>)> {
        let pool = &self.db_service.pool;
        let has_jobset =
            !crate::db::github::head_jobset_ids(head.owner, head.repo_name, head.sha, pool)
                .await?
                .is_empty();
        let request = crate::db::github::get_eval_request(head, pool).await?;
        let decision = decide(has_jobset, request.as_ref(), unix_now(), self.process_start);
        if decision == Decision::Exhausted {
            debug!(
                "Not re-queuing {}/{} head {}: reconcile attempts exhausted",
                head.owner, head.repo_name, head.sha
            );
        }
        Ok((decision, request))
    }
}

/// Log a reconciler-initiated evaluation of `head` (`what` names it,
/// e.g. `#7` or `merge group for main`).
fn log_enqueue(head: HeadCommit<'_>, what: &str, request: Option<&EvalRequest>) {
    let attempt = request.map_or(0, |r| r.attempts) + 1;
    info!(
        event = "eval_reconcile_enqueue",
        "Head {} of {}/{} ({}) was never evaluated; queuing evaluation (reconcile attempt {}/{})",
        head.sha,
        head.owner,
        head.repo_name,
        what,
        attempt,
        MAX_RECONCILE_ATTEMPTS
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: i64 = 1_000_000;

    fn request(requested_at: i64, attempts: i64, completed: bool) -> EvalRequest {
        EvalRequest {
            requested_at,
            attempts,
            completed_at: completed.then_some(requested_at),
        }
    }

    #[test]
    fn evaluated_heads_are_done() {
        assert_eq!(decide(true, None, START, START), Decision::Done);
        let finished = request(START, 0, true);
        assert_eq!(decide(false, Some(&finished), START, START), Decision::Done);
    }

    #[test]
    fn unknown_heads_are_enqueued() {
        assert_eq!(decide(false, None, START, START), Decision::Enqueue);
    }

    #[test]
    fn recent_requests_from_this_process_are_in_flight() {
        let req = request(START + 10, 0, false);
        assert_eq!(
            decide(false, Some(&req), START + 60, START),
            Decision::InFlight
        );
    }

    #[test]
    fn requests_from_before_a_restart_are_requeued() {
        let req = request(START - 10, 0, false);
        assert_eq!(
            decide(false, Some(&req), START + 60, START),
            Decision::Enqueue
        );
    }

    #[test]
    fn stale_requests_are_requeued() {
        let req = request(START, 1, false);
        let later = START + STALE_AFTER_SECS;
        assert_eq!(decide(false, Some(&req), later, START), Decision::Enqueue);
    }

    #[test]
    fn retries_are_bounded() {
        let req = request(START - 10, MAX_RECONCILE_ATTEMPTS, false);
        assert_eq!(
            decide(false, Some(&req), START + 60, START),
            Decision::Exhausted
        );
    }
}
