use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info};

use super::graph::BuildGraph;
use crate::db::DbService;
use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};
use crate::db::model::drv::Drv;
use crate::db::model::drv_id::DrvId;

/// Cached read-only node data for lockfree concurrent access
#[derive(Debug, Clone)]
pub struct CachedNode {
    pub drv_id: DrvId,
    pub system: String,
    pub is_fod: bool,
    pub build_state: DrvBuildState,
    /// Immutable shared reference to dependencies for cheap cloning
    pub dependencies: Arc<[DrvId]>,
}

impl CachedNode {
    fn from_graph_node(node: &super::graph::GraphNode) -> Self {
        Self {
            drv_id: node.drv_id.clone(),
            system: node.system.clone(),
            is_fod: node.is_fod,
            build_state: node.build_state.clone(),
            dependencies: node.dependencies.clone().into(),
        }
    }
}

/// Commands that can be sent to the GraphService
#[derive(Debug)]
pub enum GraphCommand {
    /// Update the build state of a drv
    UpdateState {
        drv_id: DrvId,
        new_state: DrvBuildState,
        response: oneshot::Sender<Result<(), GraphError>>,
    },
    /// Insert new drvs and their dependencies
    InsertDrvs {
        drvs: Vec<Drv>,
        refs: Vec<(DrvId, DrvId)>,
        response: oneshot::Sender<Result<(), GraphError>>,
    },
    /// Propagate failure from a failed drv to all transitive dependents
    PropagateFailure {
        failed_drv: DrvId,
        response: oneshot::Sender<Result<Vec<DrvId>, GraphError>>,
    },
    /// Clear failure and unblock drvs when a failed drv succeeds
    ClearFailure {
        formerly_failed: DrvId,
        response: oneshot::Sender<Result<Vec<DrvId>, GraphError>>,
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
}

/// Errors that can occur during graph operations
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("Service has shut down")]
    ServiceShutdown,
    #[error("Drv not found: {0}")]
    DrvNotFound(String),
}

/// The graph service that owns the mutable graph state
pub struct GraphService {
    graph: BuildGraph,
    shared_view: Arc<DashMap<DrvId, CachedNode>>,
    command_receiver: mpsc::Receiver<GraphCommand>,
    db_service: DbService,
}

impl GraphService {
    /// Create a new GraphService, initializing the graph from the database
    pub async fn new(
        db_service: DbService,
        command_receiver: mpsc::Receiver<GraphCommand>,
    ) -> anyhow::Result<Self> {
        info!("Initializing BuildGraph from database...");
        let graph = BuildGraph::from_database(&db_service).await?;
        info!("BuildGraph initialized with {} nodes", graph.node_count());

        // Build the shared view cache
        let shared_view = Arc::new(DashMap::new());
        for (drv_id, node) in graph.nodes.iter() {
            let cached_node = CachedNode::from_graph_node(node);
            shared_view.insert(drv_id.clone(), cached_node);
        }

        Ok(Self {
            graph,
            shared_view,
            command_receiver,
            db_service,
        })
    }

    /// Get a handle to interact with the service
    pub fn handle(&self) -> GraphServiceHandle {
        GraphServiceHandle {
            shared_view: Arc::clone(&self.shared_view),
        }
    }

    /// Run the service, processing commands until the channel closes
    pub async fn run(mut self) {
        info!("GraphService started");

        while let Some(command) = self.command_receiver.recv().await {
            if let Err(e) = self.handle_command(command).await {
                error!("Error handling graph command: {:?}", e);
            }
        }

        info!("GraphService stopped");
    }

    /// Handle a single command
    async fn handle_command(&mut self, command: GraphCommand) -> anyhow::Result<()> {
        match command {
            GraphCommand::UpdateState {
                drv_id,
                new_state,
                response,
            } => {
                debug!("UpdateState: {:?} -> {:?}", drv_id, new_state);
                self.update_state(&drv_id, new_state.clone()).await?;
                let _ = response.send(Ok(()));
            },

            GraphCommand::InsertDrvs {
                drvs,
                refs,
                response,
            } => {
                debug!("InsertDrvs: {} drvs, {} refs", drvs.len(), refs.len());
                self.insert_drvs(drvs, refs).await?;
                let _ = response.send(Ok(()));
            },

            GraphCommand::PropagateFailure {
                failed_drv,
                response,
            } => {
                debug!("PropagateFailure: {:?}", failed_drv);
                let blocked = self.propagate_failure(&failed_drv).await?;
                let _ = response.send(Ok(blocked));
            },

            GraphCommand::ClearFailure {
                formerly_failed,
                response,
            } => {
                debug!("ClearFailure: {:?}", formerly_failed);
                let unblocked = self.clear_failure(&formerly_failed).await?;
                let _ = response.send(Ok(unblocked));
            },

            GraphCommand::GetBuildableDrvs { response } => {
                let buildable = self.graph.get_drvs_by_state(&DrvBuildState::Buildable);
                let _ = response.send(buildable);
            },

            GraphCommand::GetDependents { drv_id, response } => {
                let dependents = self.graph.get_dependents(&drv_id);
                let _ = response.send(dependents);
            },
        }

        Ok(())
    }

    /// Update drv state in graph and cache
    async fn update_state(
        &mut self,
        drv_id: &DrvId,
        new_state: DrvBuildState,
    ) -> anyhow::Result<()> {
        // Update in-memory graph
        self.graph.update_state(drv_id, new_state.clone());

        // Update shared view cache
        if let Some(mut cached) = self.shared_view.get_mut(drv_id) {
            cached.build_state = new_state.clone();
        }

        // Persist terminal states to SQLite asynchronously
        if new_state.is_terminal() {
            self.db_service
                .update_drv_status(drv_id, &new_state)
                .await?;
        }

        Ok(())
    }

    /// Insert new drvs and edges into the graph
    async fn insert_drvs(
        &mut self,
        drvs: Vec<Drv>,
        refs: Vec<(DrvId, DrvId)>,
    ) -> anyhow::Result<()> {
        // Insert nodes into graph
        for drv in drvs {
            let drv_id = drv.drv_path.clone();
            self.graph.insert_node(drv);

            // Update shared view cache
            if let Some(node) = self.graph.get_node(&drv_id) {
                let cached_node = CachedNode::from_graph_node(node);
                self.shared_view.insert(drv_id, cached_node);
            }
        }

        // Insert edges
        for (referrer, reference) in refs {
            self.graph.add_edge(referrer.clone(), reference.clone());

            // Update cached dependencies for referrer
            if let Some(node) = self.graph.get_node(&referrer) {
                if let Some(mut cached) = self.shared_view.get_mut(&referrer) {
                    cached.dependencies = node.dependencies.clone().into();
                }
            }
        }

        Ok(())
    }

    /// Propagate failure to all transitive dependents
    async fn propagate_failure(&mut self, failed_drv: &DrvId) -> anyhow::Result<Vec<DrvId>> {
        let blocked = self.graph.propagate_failure(failed_drv);

        // Update shared view cache for all blocked drvs
        for blocked_id in &blocked {
            if let Some(mut cached) = self.shared_view.get_mut(blocked_id) {
                cached.build_state = DrvBuildState::TransitiveFailure;
            }
        }

        Ok(blocked)
    }

    /// Clear failure and unblock drvs
    async fn clear_failure(&mut self, formerly_failed: &DrvId) -> anyhow::Result<Vec<DrvId>> {
        let unblocked = self.graph.clear_failure(formerly_failed);

        // Update shared view cache for all unblocked drvs
        for unblocked_id in &unblocked {
            if let Some(mut cached) = self.shared_view.get_mut(unblocked_id) {
                cached.build_state = DrvBuildState::Queued;
            }
        }

        Ok(unblocked)
    }
}

/// Handle for interacting with the GraphService from other services
#[derive(Clone)]
pub struct GraphServiceHandle {
    shared_view: Arc<DashMap<DrvId, CachedNode>>,
}

impl GraphServiceHandle {
    /// Fast lockfree check if a drv is buildable
    /// This is the critical hot path - no message passing, no async
    pub fn is_buildable(&self, drv_id: &DrvId) -> bool {
        let Some(node) = self.shared_view.get(drv_id) else {
            return false;
        };

        // Check if all dependencies are successfully completed
        node.dependencies.iter().all(|dep_id| {
            self.shared_view.get(dep_id).map_or(false, |dep| {
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

    /// Check if graph contains a drv
    pub fn contains(&self, drv_id: &DrvId) -> bool {
        self.shared_view.contains_key(drv_id)
    }

    /// Get the number of nodes in the graph
    pub fn node_count(&self) -> usize {
        self.shared_view.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_graph_service_handle_is_buildable() {
        // This would require setting up a test database
        // For now, just test that the handle can be created
        let shared_view = Arc::new(DashMap::new());
        let handle = GraphServiceHandle { shared_view };

        assert_eq!(handle.node_count(), 0);
        assert!(!handle.contains(&DrvId::from_str("test-drv.drv").unwrap()));
    }
}
