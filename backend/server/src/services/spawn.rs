//! Monitored task spawning.
//!
//! [`spawn_logged`] is a drop-in replacement for `tokio::spawn` that
//! logs an `error!` if the spawned future panics. Use it for detached
//! work (debounce timers, one-shot sends, background loops) that would
//! otherwise fail silently.

use std::future::Future;

use tokio::task::JoinHandle;
use tracing::error;

/// Spawn a future on the tokio runtime and log an error if it panics.
///
/// The returned [`JoinHandle`] may be safely discarded — the panic is
/// still surfaced via the tracing log.
pub fn spawn_logged<F>(label: &'static str, future: F) -> JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let inner = tokio::spawn(future);
    tokio::spawn(async move {
        if let Err(e) = inner.await {
            if e.is_panic() {
                error!(task = label, "spawned task panicked: {:?}", e.into_panic());
            } else {
                error!(task = label, "spawned task was cancelled");
            }
        }
    })
}
