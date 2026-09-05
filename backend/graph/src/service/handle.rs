// GraphServiceHandle for interacting with the GraphService

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dashmap::DashMap;
use shared::types::{DrvBuildResult, DrvBuildState, DrvId};
use tokio::sync::{mpsc, oneshot};

use super::{CachedNode, GraphCommand};

/// Handle for interacting with the GraphService from other services
#[derive(Clone)]
pub struct GraphServiceHandle {
    pub(super) shared_view: Arc<DashMap<DrvId, CachedNode>>,
    pub(super) command_sender: mpsc::Sender<GraphCommand>,
}

impl GraphServiceHandle {
    /// Access the shared view for direct reads/writes.
    pub fn shared_view(&self) -> &Arc<DashMap<DrvId, CachedNode>> {
        &self.shared_view
    }

    /// Access the command sender for sending graph commands directly.
    pub fn command_sender(&self) -> &mpsc::Sender<GraphCommand> {
        &self.command_sender
    }

    /// Fast lockfree check if a drv is buildable.
    ///
    /// This is the critical hot path — no message passing, no async. The
    /// underlying LRU-based `BuildGraph` is _not_ touched here; non-terminal
    /// nodes are already kept alive by the explicit `pinned` set in
    /// `BuildGraph`, so there is no need to bump LRU positions from query
    /// paths.
    pub fn is_buildable(&self, drv_id: &DrvId) -> bool {
        let Some(node) = self.shared_view.get(drv_id) else {
            return false;
        };

        // Check if all dependencies are successfully completed
        node.dependencies.iter().all(|dep_id| {
            self.shared_view.get(dep_id).is_some_and(|dep| {
                dep.build_state == DrvBuildState::Completed(DrvBuildResult::Success)
            })
        })
    }

    /// Get the build state of a drv
    pub fn get_build_state(&self, drv_id: &DrvId) -> Option<DrvBuildState> {
        self.shared_view
            .get(drv_id)
            .map(|node| node.build_state.clone())
    }

    /// Get a cached node
    pub fn get_node(&self, drv_id: &DrvId) -> Option<CachedNode> {
        self.shared_view.get(drv_id).map(|node| node.clone())
    }

    /// Get the number of nodes in the graph
    pub fn node_count(&self) -> usize {
        self.shared_view.len()
    }

    /// Get direct dependents (referrers) of a drv
    pub async fn get_dependents(&self, drv_id: &DrvId) -> anyhow::Result<Vec<DrvId>> {
        let (tx, rx) = oneshot::channel();
        self.command_sender
            .send(GraphCommand::GetDependents {
                drv_id: drv_id.clone(),
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    /// Return deps that are NOT in Completed(Success) state.
    /// Used for debugging why a drv is not buildable.
    pub fn blocking_deps(&self, drv_id: &DrvId) -> Vec<(DrvId, Option<DrvBuildState>)> {
        let Some(node) = self.shared_view.get(drv_id) else {
            return vec![];
        };
        node.dependencies
            .iter()
            .filter_map(|dep_id| {
                let state = self.shared_view.get(dep_id).map(|d| d.build_state.clone());
                if state.as_ref() != Some(&DrvBuildState::Completed(DrvBuildResult::Success)) {
                    Some((dep_id.clone(), state))
                } else {
                    None
                }
            })
            .collect()
    }

    /// Get direct dependents (reverse deps) of a drv by scanning the
    /// shared_view. This is O(n) in the graph size but avoids the
    /// graph command channel, preventing deadlocks when the channel
    /// is saturated by the BFS cascade.
    pub fn get_dependents_from_view(&self, drv_id: &DrvId) -> Vec<DrvId> {
        let mut dependents = Vec::new();
        for entry in self.shared_view.iter() {
            if entry.value().dependencies.iter().any(|dep| dep == drv_id) {
                dependents.push(entry.key().clone());
            }
        }
        dependents
    }

    /// Get direct dependencies of a drv
    pub async fn get_dependencies(&self, drv_id: &DrvId) -> anyhow::Result<Vec<DrvId>> {
        let (tx, rx) = oneshot::channel();
        self.command_sender
            .send(GraphCommand::GetDependencies {
                drv_id: drv_id.clone(),
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    /// Get failed dependencies blocking a drv
    pub async fn get_failed_dependencies(&self, drv_id: &DrvId) -> anyhow::Result<Vec<DrvId>> {
        let (tx, rx) = oneshot::channel();
        self.command_sender
            .send(GraphCommand::GetFailedDependencies {
                drv_id: drv_id.clone(),
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    /// Get all buildable drvs
    pub async fn get_buildable_drvs(&self) -> anyhow::Result<Vec<DrvId>> {
        let (tx, rx) = oneshot::channel();
        self.command_sender
            .send(GraphCommand::GetBuildableDrvs { response: tx })
            .await?;
        Ok(rx.await?)
    }

    /// Get all failed drvs
    pub async fn get_all_failed_drvs(&self) -> anyhow::Result<Vec<DrvId>> {
        let (tx, rx) = oneshot::channel();
        self.command_sender
            .send(GraphCommand::GetAllFailedDrvs { response: tx })
            .await?;
        Ok(rx.await?)
    }

    /// Update the build state of a drv
    pub async fn update_state(
        &self,
        drv_id: &DrvId,
        new_state: DrvBuildState,
    ) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        self.command_sender
            .send(GraphCommand::UpdateState {
                drv_id: drv_id.clone(),
                new_state,
                response: tx,
            })
            .await?;
        rx.await?;
        Ok(())
    }

    /// Compute the union of transitive dependents reachable from any drv in
    /// `seeds`. Used by the A2 rebuild-impact endpoint to compute the total
    /// "blast radius" of a commit.
    pub async fn reverse_reachable_from_set(
        &self,
        seeds: Vec<DrvId>,
    ) -> anyhow::Result<HashSet<DrvId>> {
        let (tx, rx) = oneshot::channel();
        self.command_sender
            .send(GraphCommand::ReverseReachableFromSet {
                seeds,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }

    /// Compute, for each seed, the count of strict transitive dependents.
    /// Used by the A2 rebuild-impact endpoint for per-package "blast radius"
    /// rankings.
    pub async fn blast_radius_per_seed(
        &self,
        seeds: Vec<DrvId>,
    ) -> anyhow::Result<HashMap<DrvId, usize>> {
        let (tx, rx) = oneshot::channel();
        self.command_sender
            .send(GraphCommand::BlastRadiusPerSeed {
                seeds,
                response: tx,
            })
            .await?;
        Ok(rx.await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_graph_service_handle_is_buildable() {
        use std::str::FromStr;

        // This would require setting up a test database
        // For now, just test that the handle can be created
        let shared_view = Arc::new(DashMap::new());
        let (command_sender, _command_receiver) = mpsc::channel(10);
        let handle = GraphServiceHandle {
            shared_view,
            command_sender,
        };

        assert_eq!(handle.node_count(), 0);
        assert!(
            handle
                .get_node(&DrvId::from_str("00000000000000000000000000000000-test.drv").unwrap())
                .is_none()
        );
    }
}
