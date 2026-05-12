// GraphService command types

use std::collections::{HashMap, HashSet};

use tokio::sync::oneshot;

use shared::types::{Drv, DrvBuildState, DrvId};

/// Commands that can be sent to the GraphService.
///
/// Response channels always carry the _success_ payload for the command.
/// If the service fails to process a command, the response sender is
/// dropped and the caller observes a `RecvError` which they propagate as
/// an `anyhow::Error` via `rx.await?`. See `handle_command` for details.
#[derive(Debug)]
pub enum GraphCommand {
    /// Update the build state of a drv
    UpdateState {
        drv_id: DrvId,
        new_state: DrvBuildState,
        response: oneshot::Sender<()>,
    },
    /// Insert new drvs and their dependencies
    InsertDrvs {
        drvs: Vec<Drv>,
        refs: Vec<(DrvId, DrvId)>,
        response: oneshot::Sender<()>,
    },
    /// Propagate failure from a failed drv to all transitive dependents
    PropagateFailure {
        failed_drv: DrvId,
        response: oneshot::Sender<Vec<DrvId>>,
    },
    /// Clear failure and unblock drvs when a failed drv succeeds
    ClearFailure {
        formerly_failed: DrvId,
        response: oneshot::Sender<Vec<DrvId>>,
    },
    /// Get all drvs that are currently buildable
    GetBuildableDrvs {
        response: oneshot::Sender<Vec<DrvId>>,
    },
    /// Get direct dependents (referrers) of a drv
    GetDependents {
        drv_id: DrvId,
        response: oneshot::Sender<Vec<DrvId>>,
    },
    /// Get direct dependencies of a drv
    GetDependencies {
        drv_id: DrvId,
        response: oneshot::Sender<Vec<DrvId>>,
    },
    /// Get failed dependencies blocking a drv
    GetFailedDependencies {
        drv_id: DrvId,
        response: oneshot::Sender<Vec<DrvId>>,
    },
    /// Get all drvs that are in failed state
    GetAllFailedDrvs {
        response: oneshot::Sender<Vec<DrvId>>,
    },
    /// Compute the union of transitive dependents reachable from any of the
    /// supplied seeds. Used by the A2 rebuild-impact endpoint to answer
    /// "how many unique drvs would have to rebuild?"
    ///
    /// Returns the full reachable set (including the seeds themselves when
    /// they are present in the graph). Missing seeds are skipped silently.
    ReverseReachableFromSet {
        seeds: Vec<DrvId>,
        response: oneshot::Sender<HashSet<DrvId>>,
    },
    /// Compute, for each seed, the count of strict transitive dependents.
    /// Used by the A2 rebuild-impact endpoint for per-package "blast radius"
    /// rankings.
    ///
    /// Missing seeds map to `0`.
    BlastRadiusPerSeed {
        seeds: Vec<DrvId>,
        response: oneshot::Sender<HashMap<DrvId, usize>>,
    },
}
