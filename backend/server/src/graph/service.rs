use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

use super::eviction::{EvictionCandidateSelector, EvictionConfig};
use super::graph::BuildGraph;
use crate::db::DbService;
use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};
use crate::db::model::drv::Drv;
use crate::db::model::drv_id::DrvId;
use crate::metrics::GraphMetrics;

/// Cached read-only node data for lockfree concurrent access
#[derive(Debug, Clone)]
pub struct CachedNode {
    pub drv_id: DrvId,
    pub system: String,
    pub required_system_features: Option<String>,
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
            required_system_features: node.required_system_features.clone(),
            is_fod: node.is_fod,
            build_state: node.build_state.clone(),
            dependencies: node.dependencies.clone().into(),
        }
    }

    /// Convert CachedNode to Drv
    /// Note: prefer_local_build is always false as it's not persisted in DB
    pub fn to_drv(&self) -> Drv {
        Drv {
            drv_path: self.drv_id.clone(),
            system: self.system.clone(),
            prefer_local_build: false,
            required_system_features: self.required_system_features.clone(),
            is_fod: self.is_fod,
            build_state: self.build_state.clone(),
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
    /// Track last access time for LRU eviction policy
    last_accessed: HashMap<DrvId, Instant>,
    /// Reference counts: how many in-cache nodes depend on this node
    ref_counts: HashMap<DrvId, usize>,
    /// Metrics for observability
    metrics: Option<Arc<GraphMetrics>>,
    /// Eviction candidate selector
    eviction_selector: EvictionCandidateSelector,
    /// Last time we ran dry-run eviction check
    last_dry_run_check: Instant,
}

impl GraphService {
    /// Create a new GraphService, initializing the graph from the database
    pub async fn new(
        db_service: DbService,
        command_receiver: mpsc::Receiver<GraphCommand>,
        metrics: Option<Arc<GraphMetrics>>,
    ) -> anyhow::Result<Self> {
        info!("Initializing BuildGraph from database...");
        let graph = BuildGraph::from_database(&db_service).await?;
        info!("BuildGraph initialized with {} nodes", graph.node_count());

        // Build the shared view cache
        let shared_view = Arc::new(DashMap::new());
        let now = Instant::now();
        let mut last_accessed = HashMap::new();
        let mut ref_counts = HashMap::new();

        for (drv_id, node) in graph.nodes.iter() {
            let cached_node = CachedNode::from_graph_node(node);
            shared_view.insert(drv_id.clone(), cached_node);

            // Initialize last_accessed to now for all nodes
            last_accessed.insert(drv_id.clone(), now);

            // Calculate initial ref_counts
            for dep_id in &node.dependencies {
                *ref_counts.entry(dep_id.clone()).or_insert(0) += 1;
            }
        }

        let now = Instant::now();
        let service = Self {
            graph,
            shared_view,
            command_receiver,
            db_service,
            last_accessed,
            ref_counts,
            metrics,
            eviction_selector: EvictionCandidateSelector::new(EvictionConfig::default()),
            last_dry_run_check: now,
        };

        // Update initial metrics
        service.update_metrics();

        Ok(service)
    }

    /// Get a handle to interact with the service
    pub fn handle(&self, command_sender: mpsc::Sender<GraphCommand>) -> GraphServiceHandle {
        GraphServiceHandle {
            shared_view: Arc::clone(&self.shared_view),
            command_sender,
        }
    }

    /// Run the service, processing commands until the channel closes or cancellation
    pub async fn run(mut self, cancellation_token: CancellationToken) {
        info!("GraphService started");

        while let Some(command) = cancellation_token
            .run_until_cancelled(self.command_receiver.recv())
            .await
            .flatten()
        {
            if let Err(e) = self.handle_command(command).await {
                error!("Error handling graph command: {:?}", e);
            }

            // Periodically check eviction candidates (dry-run mode)
            self.maybe_dry_run_eviction_check();
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
                // Ensure node is loaded (may have been evicted)
                if let Err(e) = self.ensure_loaded(&drv_id).await {
                    error!("Failed to ensure loaded for GetDependents: {:?}", e);
                    let _ = response.send(Vec::new());
                } else {
                    let dependents = self.graph.get_dependents(&drv_id);
                    let _ = response.send(dependents);
                }
            },

            GraphCommand::GetDependencies { drv_id, response } => {
                // Ensure node is loaded (may have been evicted)
                if let Err(e) = self.ensure_loaded(&drv_id).await {
                    error!("Failed to ensure loaded for GetDependencies: {:?}", e);
                    let _ = response.send(Vec::new());
                } else {
                    let dependencies = self.graph.get_dependencies(&drv_id);
                    let _ = response.send(dependencies);
                }
            },

            GraphCommand::GetFailedDependencies { drv_id, response } => {
                // Ensure node is loaded (may have been evicted)
                if let Err(e) = self.ensure_loaded(&drv_id).await {
                    error!("Failed to ensure loaded for GetFailedDependencies: {:?}", e);
                    let _ = response.send(Vec::new());
                } else {
                    let failed_deps = self.graph.get_failed_dependencies(&drv_id);
                    let _ = response.send(failed_deps);
                }
            },
        }

        Ok(())
    }

    /// Ensure a node is loaded in the cache, reloading from DB if evicted
    /// Returns true if the node was reloaded, false if it was already in cache
    async fn ensure_loaded(&mut self, drv_id: &DrvId) -> anyhow::Result<bool> {
        // Check if node is already in cache (use contains for efficient check)
        if self.graph.nodes.contains(drv_id) {
            return Ok(false);
        }

        debug!("Cache miss: reloading {:?} from database", drv_id);

        // Load the drv from database
        let Some(drv) = self.db_service.get_drv(drv_id).await? else {
            anyhow::bail!("Drv not found in database: {:?}", drv_id);
        };

        // Load all edges where this drv is involved
        let pool = &self.db_service.pool;

        // Load dependencies (where this drv is the referrer)
        let deps: Vec<(String,)> = sqlx::query_as(
            "SELECT reference FROM DrvRefs WHERE referrer = ?",
        )
        .bind(drv_id)
        .fetch_all(pool)
        .await?;

        // Load dependents (where this drv is the reference)
        let dependents: Vec<(String,)> = sqlx::query_as(
            "SELECT referrer FROM DrvRefs WHERE reference = ?",
        )
        .bind(drv_id)
        .fetch_all(pool)
        .await?;

        // Insert the node
        let now = Instant::now();
        self.graph.insert_node(drv);
        self.last_accessed.insert(drv_id.clone(), now);

        // Re-add edges
        for (dep_str,) in deps {
            let dep_id: DrvId = std::str::FromStr::from_str(&dep_str)?;
            self.graph.add_edge(drv_id.clone(), dep_id.clone());

            // Update ref_count
            *self.ref_counts.entry(dep_id).or_insert(0) += 1;
        }

        for (dependent_str,) in dependents {
            let dependent_id: DrvId = std::str::FromStr::from_str(&dependent_str)?;
            self.graph.add_edge(dependent_id.clone(), drv_id.clone());

            // Update ref_count
            *self.ref_counts.entry(drv_id.clone()).or_insert(0) += 1;
        }

        // Update shared view cache
        if let Some(node) = self.graph.get_node(drv_id) {
            let cached_node = CachedNode::from_graph_node(node);
            self.shared_view.insert(drv_id.clone(), cached_node);
        }

        // Update metrics if configured
        if self.metrics.is_some() {
            self.update_metrics();
        }

        Ok(true)
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

        // Persist all state changes to SQLite
        self.db_service
            .update_drv_status(drv_id, &new_state)
            .await?;

        Ok(())
    }

    /// Insert new drvs and edges into the graph
    async fn insert_drvs(
        &mut self,
        drvs: Vec<Drv>,
        refs: Vec<(DrvId, DrvId)>,
    ) -> anyhow::Result<()> {
        let now = Instant::now();

        // Insert nodes into graph
        for drv in drvs {
            let drv_id = drv.drv_path.clone();
            self.graph.insert_node(drv);

            // Initialize last_accessed
            self.last_accessed.insert(drv_id.clone(), now);

            // Update shared view cache
            if let Some(node) = self.graph.get_node(&drv_id) {
                let cached_node = CachedNode::from_graph_node(node);
                self.shared_view.insert(drv_id, cached_node);
            }
        }

        // Insert edges
        for (referrer, reference) in refs {
            self.graph.add_edge(referrer.clone(), reference.clone());

            // Update ref_count: reference now has one more dependent
            *self.ref_counts.entry(reference.clone()).or_insert(0) += 1;

            // Update cached dependencies for referrer
            if let Some(node) = self.graph.get_node(&referrer) {
                if let Some(mut cached) = self.shared_view.get_mut(&referrer) {
                    cached.dependencies = node.dependencies.clone().into();
                }
            }
        }

        // Update metrics after bulk insert
        self.update_metrics();

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

        // Persist transitive failures to database
        if !blocked.is_empty() {
            self.db_service
                .insert_transitive_failures(failed_drv, &blocked)
                .await?;
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

        // Persist clearing of transitive failures to database
        self.db_service
            .clear_transitive_failures(formerly_failed)
            .await?;

        Ok(unblocked)
    }

    /// Update Prometheus metrics based on current graph state
    fn update_metrics(&self) {
        let Some(ref metrics) = self.metrics else {
            return;
        };

        // Update memory estimate
        let memory_bytes = self.graph.estimate_memory_bytes();
        metrics.memory_bytes_estimate.set(memory_bytes as f64);

        // Update node counts by state
        use DrvBuildState::*;
        let states = vec![
            Queued,
            Buildable,
            Building,
            FailedRetry,
            Blocked,
            TransitiveFailure,
            Completed(DrvBuildResult::Success),
            Completed(DrvBuildResult::Failure),
        ];

        for state in states {
            let count = self.graph.get_drvs_by_state(&state).len();
            let state_label = format!("{:?}", state);
            metrics
                .nodes_total
                .with_label_values(&[&state_label])
                .set(count as f64);
        }

        // Update ref_count histogram
        for (drv_id, &ref_count) in &self.ref_counts {
            let has_dependents = if let Some(node) = self.graph.get_node(drv_id) {
                if node.dependents.is_empty() {
                    "false"
                } else {
                    "true"
                }
            } else {
                "unknown"
            };

            metrics
                .ref_count_histogram
                .with_label_values(&[has_dependents])
                .observe(ref_count as f64);
        }

        // Update eviction candidate counts by tier
        let candidate_counts = self.eviction_selector.count_candidates_by_tier(
            &self.graph,
            &self.last_accessed,
            &self.ref_counts,
        );

        for (tier, count) in candidate_counts {
            metrics
                .eviction_candidates_total
                .with_label_values(&[tier.name()])
                .set(count as f64);
        }
    }

    /// Perform a dry-run eviction check and log what would be evicted
    /// This runs periodically to validate eviction policy without actually evicting
    fn maybe_dry_run_eviction_check(&mut self) {
        let now = Instant::now();
        let since_last_check = now.duration_since(self.last_dry_run_check);

        // Run check every 5 minutes
        if since_last_check < std::time::Duration::from_secs(300) {
            return;
        }

        self.last_dry_run_check = now;

        // Check what would be evicted if we needed to free up 20% of capacity
        let current_count = self.graph.node_count();
        let target_evict = current_count / 5; // 20%

        if target_evict == 0 {
            return;
        }

        let candidates = self.eviction_selector.select_candidates(
            &self.graph,
            &self.last_accessed,
            &self.ref_counts,
            target_evict,
        );

        if candidates.is_empty() {
            info!(
                "Dry-run eviction check: No candidates available despite {} nodes in graph",
                current_count
            );
            return;
        }

        // Count candidates by tier
        let mut tier1_count = 0;
        let mut tier2_count = 0;
        let mut tier3_count = 0;

        for candidate in &candidates {
            match candidate.tier {
                super::eviction::EvictionTier::Tier1 => tier1_count += 1,
                super::eviction::EvictionTier::Tier2 => tier2_count += 1,
                super::eviction::EvictionTier::Tier3 => tier3_count += 1,
            }
        }

        info!(
            "Dry-run eviction check: Would evict {} nodes ({}% of total) - \
             Tier1: {}, Tier2: {}, Tier3: {}",
            candidates.len(),
            (candidates.len() * 100) / current_count,
            tier1_count,
            tier2_count,
            tier3_count
        );

        // Log details of oldest candidate from each tier (for debugging)
        let tier1_oldest = candidates
            .iter()
            .find(|c| c.tier == super::eviction::EvictionTier::Tier1);
        let tier2_oldest = candidates
            .iter()
            .find(|c| c.tier == super::eviction::EvictionTier::Tier2);
        let tier3_oldest = candidates
            .iter()
            .find(|c| c.tier == super::eviction::EvictionTier::Tier3);

        if let Some(candidate) = tier1_oldest {
            debug!(
                "  Tier1 (TransitiveFailure) oldest: {:?}, age: {:.1}h, ref_count: {}",
                candidate.drv_id,
                candidate.age.as_secs_f64() / 3600.0,
                candidate.ref_count
            );
        }

        if let Some(candidate) = tier2_oldest {
            debug!(
                "  Tier2 (Completed(Failure)) oldest: {:?}, age: {:.1}h, ref_count: {}",
                candidate.drv_id,
                candidate.age.as_secs_f64() / 3600.0,
                candidate.ref_count
            );
        }

        if let Some(candidate) = tier3_oldest {
            debug!(
                "  Tier3 (Completed(Success)) oldest: {:?}, age: {:.1}h, ref_count: {}",
                candidate.drv_id,
                candidate.age.as_secs_f64() / 3600.0,
                candidate.ref_count
            );
        }

        // Validate that all candidates have ref_count == 0
        let invalid_candidates: Vec<_> = candidates
            .iter()
            .filter(|c| c.ref_count != 0)
            .collect();

        if !invalid_candidates.is_empty() {
            error!(
                "BUG: Dry-run eviction found {} candidates with ref_count > 0!",
                invalid_candidates.len()
            );
            for candidate in invalid_candidates {
                error!(
                    "  Invalid candidate: {:?}, ref_count: {}",
                    candidate.drv_id, candidate.ref_count
                );
            }
        }
    }
}

/// Handle for interacting with the GraphService from other services
#[derive(Clone)]
pub struct GraphServiceHandle {
    shared_view: Arc<DashMap<DrvId, CachedNode>>,
    command_sender: mpsc::Sender<GraphCommand>,
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
        rx.await??;
        Ok(())
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
        assert!(!handle.contains(&DrvId::from_str("test-drv.drv").unwrap()));
    }
}
