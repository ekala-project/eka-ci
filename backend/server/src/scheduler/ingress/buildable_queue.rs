//! Lossless, deduplicated queue of buildability re-checks.
//!
//! When a build succeeds the recorder re-checks every referrer of the drv
//! (and every drv the success unblocked). These re-checks used to go
//! through the bounded ingress channel with `try_send`, so whenever the
//! channel was full (e.g. while an eval floods it with `EvalRequest`s) they
//! were dropped and the drvs sat `Queued` until the periodic sweep found
//! them. Blocking sends were not an option either: ingress itself blocks on
//! the recorder channel for cache hits, so recorder → ingress back-pressure
//! could deadlock.
//!
//! [`BuildabilityQueue`] never blocks and never drops: pushes go into an
//! in-memory FIFO and a drv already pending is not queued twice (a drv with
//! many deps is re-checked once per finished dep). This matches the spec's
//! `ingress_queue: Set[DrvId]` (`inv_buildable_check_pending`).
//!
//! The queue is not journaled. Its contents are lost on a crash and
//! recovered at startup, which re-checks every Buildable drv and every
//! Queued drv whose deps have all succeeded.

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tracing::warn;

use super::IngressWorker;
use crate::db::model::drv_id::DrvId;

/// Re-checks handled per wake-up, so `EvalRequest`s on the ingress channel
/// keep being served while a large batch of re-checks is pending.
const DRAIN_BATCH: usize = 512;

#[derive(Default)]
struct Pending {
    order: VecDeque<Arc<DrvId>>,
    members: HashSet<Arc<DrvId>>,
}

/// Cloneable handle to the shared re-check queue.
#[derive(Clone, Default)]
pub struct BuildabilityQueue {
    pending: Arc<Mutex<Pending>>,
    notify: Arc<Notify>,
}

impl BuildabilityQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a buildability re-check of `drv` unless one is already pending.
    /// Never blocks.
    pub fn push(&self, drv: Arc<DrvId>) {
        let added = {
            let mut pending = self.lock();
            if pending.members.insert(Arc::clone(&drv)) {
                pending.order.push_back(drv);
                true
            } else {
                false
            }
        };
        if added {
            self.notify.notify_one();
        }
    }

    /// Number of pending re-checks.
    pub fn len(&self) -> usize {
        self.lock().order.len()
    }

    /// Whether no re-check is pending.
    pub fn is_empty(&self) -> bool {
        self.lock().order.is_empty()
    }

    /// Wait until a re-check may be pending.
    pub(super) async fn notified(&self) {
        self.notify.notified().await;
    }

    /// Take up to `max` pending re-checks, oldest first. A drv taken here is
    /// no longer pending, so pushing it again queues a new re-check.
    fn pop_batch(&self, max: usize) -> Vec<Arc<DrvId>> {
        let mut pending = self.lock();
        let n = max.min(pending.order.len());
        let batch: Vec<_> = pending.order.drain(..n).collect();
        for drv in &batch {
            pending.members.remove(drv);
        }
        batch
    }

    /// Wake the consumer again (used when a drain left items behind).
    fn renotify(&self) {
        self.notify.notify_one();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        // The critical sections cannot panic halfway through an update, so
        // a poisoned lock still guards consistent data.
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl IngressWorker {
    /// Handle one batch of pending buildability re-checks.
    pub(super) async fn drain_buildable(&self) {
        let batch = self.buildability.pop_batch(DRAIN_BATCH);
        for drv in &batch {
            if let Err(e) = self.handle_check_buildable_task(drv).await {
                warn!("buildability check of {}: {:?}", drv.store_path(), e);
            }
        }
        if !self.buildability.is_empty() {
            self.buildability.renotify();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn drv(name: &str) -> Arc<DrvId> {
        Arc::new(DrvId::from_str(&format!("/nix/store/{}-{name}.drv", "a".repeat(32))).unwrap())
    }

    #[test]
    fn pending_drv_is_not_queued_twice() {
        let queue = BuildabilityQueue::new();
        queue.push(drv("a"));
        queue.push(drv("b"));
        queue.push(drv("a"));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.pop_batch(10), vec![drv("a"), drv("b")]);
    }

    #[test]
    fn popped_drv_can_be_queued_again() {
        let queue = BuildabilityQueue::new();
        queue.push(drv("a"));
        assert_eq!(queue.pop_batch(10), vec![drv("a")]);
        queue.push(drv("a"));
        assert_eq!(queue.pop_batch(10), vec![drv("a")]);
    }

    #[test]
    fn batches_are_fifo_and_bounded() {
        let queue = BuildabilityQueue::new();
        for name in ["a", "b", "c"] {
            queue.push(drv(name));
        }
        assert_eq!(queue.pop_batch(2), vec![drv("a"), drv("b")]);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.pop_batch(2), vec![drv("c")]);
        assert!(queue.pop_batch(2).is_empty());
    }

    #[test]
    fn push_never_blocks_past_any_bound() {
        let queue = BuildabilityQueue::new();
        for i in 0..100_000 {
            queue.push(drv(&format!("d{i}")));
        }
        assert_eq!(queue.len(), 100_000);
    }

    #[tokio::test]
    async fn push_wakes_a_waiting_consumer() {
        let queue = BuildabilityQueue::new();
        let waiter = queue.clone();
        let handle = tokio::spawn(async move { waiter.notified().await });
        tokio::task::yield_now().await;
        queue.push(drv("a"));
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("consumer was not woken")
            .unwrap();
    }

    #[tokio::test]
    async fn push_before_waiting_is_not_lost() {
        let queue = BuildabilityQueue::new();
        queue.push(drv("a"));
        tokio::time::timeout(std::time::Duration::from_secs(1), queue.notified())
            .await
            .expect("stored wake-up was lost");
    }
}
