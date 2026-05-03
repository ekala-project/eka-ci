// Graph service for managing build dependency graph

mod cached_node;
mod commands;
pub mod handle;
mod metrics;
mod operations;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

pub use cached_node::CachedNode;
pub use commands::GraphCommand;
use dashmap::DashMap;
pub use handle::GraphServiceHandle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::db::DbService;
use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};
use crate::db::model::drv_id::DrvId;
use crate::graph::eviction::EvictionCandidateSelector;
use crate::graph::graph::BuildGraph;
use crate::metrics::GraphMetrics;

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
        lru_capacity: usize,
    ) -> anyhow::Result<Self> {
        info!(
            "Initializing BuildGraph from database with LRU capacity: {}",
            lru_capacity
        );
        let graph = BuildGraph::from_database(&db_service, lru_capacity).await?;
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
            eviction_selector: EvictionCandidateSelector::with_defaults(),
            last_dry_run_check: now,
        };

        // Update initial metrics
        metrics::update_metrics(
            &service.graph,
            &service.ref_counts,
            &service.eviction_selector,
            &service.last_accessed,
            service.metrics.as_ref(),
        );

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
            metrics::maybe_dry_run_eviction_check(
                &self.graph,
                &self.last_accessed,
                &self.ref_counts,
                &self.eviction_selector,
                &mut self.last_dry_run_check,
            );
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
                operations::update_state(
                    &drv_id,
                    new_state,
                    &mut self.graph,
                    &self.shared_view,
                    &self.db_service,
                )
                .await?;
                if response.send(()).is_err() {
                    warn!(
                        "UpdateState caller dropped the response oneshot for {:?}",
                        drv_id
                    );
                }
            },

            GraphCommand::InsertDrvs {
                drvs,
                refs,
                response,
            } => {
                debug!("InsertDrvs: {} drvs, {} refs", drvs.len(), refs.len());
                let (drv_count, ref_count) = (drvs.len(), refs.len());
                operations::insert_drvs(
                    drvs,
                    refs,
                    &mut self.graph,
                    &self.shared_view,
                    &mut self.last_accessed,
                    &mut self.ref_counts,
                    self.metrics.as_ref(),
                )
                .await?;

                // Update metrics after bulk insert
                metrics::update_metrics(
                    &self.graph,
                    &self.ref_counts,
                    &self.eviction_selector,
                    &self.last_accessed,
                    self.metrics.as_ref(),
                );

                if response.send(()).is_err() {
                    warn!(
                        "InsertDrvs caller dropped the response oneshot ({} drvs, {} refs)",
                        drv_count, ref_count
                    );
                }
            },

            GraphCommand::PropagateFailure {
                failed_drv,
                response,
            } => {
                debug!("PropagateFailure: {:?}", failed_drv);
                let blocked = operations::propagate_failure(
                    &failed_drv,
                    &mut self.graph,
                    &self.shared_view,
                    &self.db_service,
                )
                .await?;
                if response.send(blocked).is_err() {
                    warn!(
                        "PropagateFailure caller dropped the response oneshot for {:?}",
                        failed_drv
                    );
                }
            },

            GraphCommand::ClearFailure {
                formerly_failed,
                response,
            } => {
                debug!("ClearFailure: {:?}", formerly_failed);
                let unblocked = operations::clear_failure(
                    &formerly_failed,
                    &mut self.graph,
                    &self.shared_view,
                    &self.db_service,
                )
                .await?;
                if response.send(unblocked).is_err() {
                    warn!(
                        "ClearFailure caller dropped the response oneshot for {:?}",
                        formerly_failed
                    );
                }
            },

            GraphCommand::GetBuildableDrvs { response } => {
                let buildable = self.graph.get_drvs_by_state(&DrvBuildState::Buildable);
                if response.send(buildable).is_err() {
                    warn!("GetBuildableDrvs caller dropped the response oneshot");
                }
            },

            GraphCommand::GetDependents { drv_id, response } => {
                // Ensure node is loaded (may have been evicted)
                let reply = if let Err(e) = operations::ensure_loaded(
                    &drv_id,
                    &mut self.graph,
                    &self.shared_view,
                    &mut self.last_accessed,
                    &mut self.ref_counts,
                    &self.db_service,
                    self.metrics.as_ref(),
                )
                .await
                {
                    error!("Failed to ensure loaded for GetDependents: {:?}", e);
                    Vec::new()
                } else {
                    self.graph.get_dependents(&drv_id)
                };

                // Update metrics if we loaded anything
                if self.metrics.is_some() {
                    metrics::update_metrics(
                        &self.graph,
                        &self.ref_counts,
                        &self.eviction_selector,
                        &self.last_accessed,
                        self.metrics.as_ref(),
                    );
                }

                if response.send(reply).is_err() {
                    warn!(
                        "GetDependents caller dropped the response oneshot for {:?}",
                        drv_id
                    );
                }
            },

            GraphCommand::GetDependencies { drv_id, response } => {
                // Ensure node is loaded (may have been evicted)
                let reply = if let Err(e) = operations::ensure_loaded(
                    &drv_id,
                    &mut self.graph,
                    &self.shared_view,
                    &mut self.last_accessed,
                    &mut self.ref_counts,
                    &self.db_service,
                    self.metrics.as_ref(),
                )
                .await
                {
                    error!("Failed to ensure loaded for GetDependencies: {:?}", e);
                    Vec::new()
                } else {
                    self.graph.get_dependencies(&drv_id)
                };

                // Update metrics if we loaded anything
                if self.metrics.is_some() {
                    metrics::update_metrics(
                        &self.graph,
                        &self.ref_counts,
                        &self.eviction_selector,
                        &self.last_accessed,
                        self.metrics.as_ref(),
                    );
                }

                if response.send(reply).is_err() {
                    warn!(
                        "GetDependencies caller dropped the response oneshot for {:?}",
                        drv_id
                    );
                }
            },

            GraphCommand::GetFailedDependencies { drv_id, response } => {
                // Ensure node is loaded (may have been evicted)
                let reply = if let Err(e) = operations::ensure_loaded(
                    &drv_id,
                    &mut self.graph,
                    &self.shared_view,
                    &mut self.last_accessed,
                    &mut self.ref_counts,
                    &self.db_service,
                    self.metrics.as_ref(),
                )
                .await
                {
                    error!("Failed to ensure loaded for GetFailedDependencies: {:?}", e);
                    Vec::new()
                } else {
                    self.graph.get_failed_dependencies(&drv_id)
                };

                // Update metrics if we loaded anything
                if self.metrics.is_some() {
                    metrics::update_metrics(
                        &self.graph,
                        &self.ref_counts,
                        &self.eviction_selector,
                        &self.last_accessed,
                        self.metrics.as_ref(),
                    );
                }

                if response.send(reply).is_err() {
                    warn!(
                        "GetFailedDependencies caller dropped the response oneshot for {:?}",
                        drv_id
                    );
                }
            },

            GraphCommand::GetAllFailedDrvs { response } => {
                let failed = self
                    .graph
                    .get_drvs_by_state(&DrvBuildState::Completed(DrvBuildResult::Failure));
                if response.send(failed).is_err() {
                    warn!("GetAllFailedDrvs caller dropped the response oneshot");
                }
            },

            GraphCommand::ReverseReachableFromSet { seeds, response } => {
                debug!("ReverseReachableFromSet: {} seeds", seeds.len());
                // Best-effort reload of each seed; missing-from-DB seeds
                // simply contribute nothing to the BFS (matches BuildGraph
                // semantics).
                for seed in &seeds {
                    if let Err(e) = operations::ensure_loaded(
                        seed,
                        &mut self.graph,
                        &self.shared_view,
                        &mut self.last_accessed,
                        &mut self.ref_counts,
                        &self.db_service,
                        self.metrics.as_ref(),
                    )
                    .await
                    {
                        debug!(
                            "ReverseReachableFromSet: ensure_loaded skipped {:?}: {:?}",
                            seed, e
                        );
                    }
                }
                let reachable = self.graph.reverse_reachable_from_set(&seeds);

                // Update metrics if we loaded anything
                if self.metrics.is_some() {
                    metrics::update_metrics(
                        &self.graph,
                        &self.ref_counts,
                        &self.eviction_selector,
                        &self.last_accessed,
                        self.metrics.as_ref(),
                    );
                }

                if response.send(reachable).is_err() {
                    warn!(
                        "ReverseReachableFromSet caller dropped the response oneshot ({} seeds)",
                        seeds.len()
                    );
                }
            },

            GraphCommand::BlastRadiusPerSeed { seeds, response } => {
                debug!("BlastRadiusPerSeed: {} seeds", seeds.len());
                for seed in &seeds {
                    if let Err(e) = operations::ensure_loaded(
                        seed,
                        &mut self.graph,
                        &self.shared_view,
                        &mut self.last_accessed,
                        &mut self.ref_counts,
                        &self.db_service,
                        self.metrics.as_ref(),
                    )
                    .await
                    {
                        debug!(
                            "BlastRadiusPerSeed: ensure_loaded skipped {:?}: {:?}",
                            seed, e
                        );
                    }
                }
                let radii = self.graph.blast_radius_per_seed(&seeds);

                // Update metrics if we loaded anything
                if self.metrics.is_some() {
                    metrics::update_metrics(
                        &self.graph,
                        &self.ref_counts,
                        &self.eviction_selector,
                        &self.last_accessed,
                        self.metrics.as_ref(),
                    );
                }

                if response.send(radii).is_err() {
                    warn!(
                        "BlastRadiusPerSeed caller dropped the response oneshot ({} seeds)",
                        seeds.len()
                    );
                }
            },
        }

        Ok(())
    }
}
