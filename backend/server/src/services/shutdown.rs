//! Bounded wait for service tasks after the shared cancellation token
//! has fired.
//!
//! Each service is expected to observe the token and exit promptly, but
//! a single service that ignores it (or is stuck in a non-cancellable
//! await) must not keep the process alive until systemd escalates to
//! SIGKILL. After [`SHUTDOWN_DEADLINE`] any service still running is
//! named in the log and aborted; aborting drops its in-flight futures,
//! which kills `kill_on_drop` subprocesses (nix, nix-store, nix-build).

use std::collections::BTreeSet;
use std::time::Duration;

use tokio::task::{AbortHandle, JoinHandle, JoinSet};
use tracing::{error, info, warn};

/// How long to wait for services to exit after cancellation. Kept well
/// below systemd's default `TimeoutStopSec` (90s) so the process exits
/// on its own terms.
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(30);

/// Named service task handles awaited during shutdown.
#[derive(Default)]
pub struct ServiceHandles {
    handles: Vec<(&'static str, JoinHandle<()>)>,
}

impl ServiceHandles {
    pub fn push(&mut self, name: &'static str, handle: JoinHandle<()>) {
        self.handles.push((name, handle));
    }

    pub fn push_opt(&mut self, name: &'static str, handle: Option<JoinHandle<()>>) {
        if let Some(handle) = handle {
            self.push(name, handle);
        }
    }

    /// Wait for every service to finish, aborting stragglers after
    /// `deadline`.
    pub async fn join(self, deadline: Duration) {
        let mut pending: BTreeSet<&'static str> = BTreeSet::new();
        let mut aborts: Vec<AbortHandle> = Vec::new();
        let mut set = JoinSet::new();
        for (name, handle) in self.handles {
            pending.insert(name);
            aborts.push(handle.abort_handle());
            set.spawn(async move { (name, handle.await) });
        }

        let wait_all = async {
            while let Some(joined) = set.join_next().await {
                match joined {
                    Ok((name, result)) => {
                        pending.remove(name);
                        if let Err(e) = result {
                            error!("{} task exited with error: {:?}", name, e);
                        }
                    },
                    Err(e) => error!("shutdown waiter failed: {:?}", e),
                }
            }
        };

        if tokio::time::timeout(deadline, wait_all).await.is_ok() {
            info!("All service tasks exited");
            return;
        }

        warn!(
            "Services did not exit within {:?}, aborting: {:?}",
            deadline, pending
        );
        for abort in aborts {
            abort.abort();
        }
    }
}
