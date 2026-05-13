// Core graph operations

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use dashmap::DashMap;
use tracing::debug;

use shared::types::{Drv, DrvBuildState, DrvId};

use crate::graph::BuildGraph;
use crate::traits::{GraphDatabase, GraphMetricsCollector};

use super::CachedNode;

/// Ensure a node is loaded in the cache, reloading from DB if evicted
/// Returns true if the node was reloaded, false if it was already in cache
pub(super) async fn ensure_loaded(
    drv_id: &DrvId,
    graph: &mut BuildGraph,
    shared_view: &Arc<DashMap<DrvId, CachedNode>>,
    last_accessed: &mut HashMap<DrvId, Instant>,
    ref_counts: &mut HashMap<DrvId, usize>,
    db: &dyn GraphDatabase,
    metrics: Option<&dyn GraphMetricsCollector>,
) -> Result<bool> {
    // Check if node is already in cache (use contains for efficient check)
    if graph.nodes.contains(drv_id) {
        // Cache hit - node is already in memory
        if let Some(metrics) = metrics {
            metrics.increment_cache_hits();
        }
        return Ok(false);
    }

    // Cache miss - node was evicted or never loaded
    debug!("Cache miss: reloading {:?} from database", drv_id);
    if let Some(metrics) = metrics {
        metrics.increment_cache_misses();
    }

    // Record cache reload metrics
    let reload_start = Instant::now();
    if let Some(metrics) = metrics {
        metrics.increment_cache_reloads();
    }

    // Load the drv from database
    let Some(drv) = db.get_drv(drv_id).await? else {
        anyhow::bail!("Drv not found in database: {:?}", drv_id);
    };

    // Load all edges where this drv is involved using trait methods
    let deps = db.get_drv_refs(drv_id).await?;
    let dependents = db.get_drv_dependents(drv_id).await?;

    // Insert the node and track potential eviction
    let now = Instant::now();
    if let Some((evicted_id, _evicted_node)) = graph.insert_node(drv) {
        // Record eviction metric
        if let Some(metrics) = metrics {
            metrics.increment_evictions();
        }

        // Clean up tracking data for evicted node
        last_accessed.remove(&evicted_id);
        ref_counts.remove(&evicted_id);
        shared_view.remove(&evicted_id);
    }
    last_accessed.insert(drv_id.clone(), now);

    // Re-add edges
    for dep_id in deps {
        graph.add_edge(drv_id.clone(), dep_id.clone());

        // Update ref_count
        *ref_counts.entry(dep_id).or_insert(0) += 1;
    }

    for dependent_id in dependents {
        graph.add_edge(dependent_id.clone(), drv_id.clone());

        // Update ref_count
        *ref_counts.entry(drv_id.clone()).or_insert(0) += 1;
    }

    // Update shared view cache
    if let Some(node) = graph.get_node(drv_id) {
        let cached_node = CachedNode::from_graph_node(node);
        shared_view.insert(drv_id.clone(), cached_node);
    }

    // Record reload duration
    if let Some(metrics) = metrics {
        let reload_duration = reload_start.elapsed();
        metrics.observe_cache_reload_duration(reload_duration.as_secs_f64());
    }

    Ok(true)
}

/// Update drv state in graph and cache
pub(super) async fn update_state(
    drv_id: &DrvId,
    new_state: DrvBuildState,
    graph: &mut BuildGraph,
    shared_view: &Arc<DashMap<DrvId, CachedNode>>,
    db: &dyn GraphDatabase,
) -> Result<()> {
    // In-memory graph update takes the first clone.
    graph.update_state(drv_id, new_state.clone());

    // Shared view cache and DB persistence share the original.
    if let Some(mut cached) = shared_view.get_mut(drv_id) {
        cached.build_state = new_state.clone();
    }

    db.update_drv_status(drv_id, &new_state).await?;

    Ok(())
}

/// Insert new drvs and edges into the graph
pub(super) async fn insert_drvs(
    drvs: Vec<Drv>,
    refs: Vec<(DrvId, DrvId)>,
    graph: &mut BuildGraph,
    shared_view: &Arc<DashMap<DrvId, CachedNode>>,
    last_accessed: &mut HashMap<DrvId, Instant>,
    ref_counts: &mut HashMap<DrvId, usize>,
    metrics: Option<&dyn GraphMetricsCollector>,
) -> Result<()> {
    let now = Instant::now();

    // Insert nodes into graph
    for drv in drvs {
        let drv_id = drv.drv_path.clone();

        // Insert node and track evictions
        if let Some((evicted_id, _evicted_node)) = graph.insert_node(drv) {
            // Record eviction metric
            if let Some(metrics) = metrics {
                metrics.increment_evictions();
            }

            // Clean up tracking data for evicted node
            last_accessed.remove(&evicted_id);
            ref_counts.remove(&evicted_id);
            shared_view.remove(&evicted_id);
        }

        // Initialize last_accessed
        last_accessed.insert(drv_id.clone(), now);

        // Update shared view cache
        if let Some(node) = graph.get_node(&drv_id) {
            let cached_node = CachedNode::from_graph_node(node);
            shared_view.insert(drv_id, cached_node);
        }
    }

    // Insert edges
    for (referrer, reference) in refs {
        graph.add_edge(referrer.clone(), reference.clone());

        // Update ref_count: reference now has one more dependent
        *ref_counts.entry(reference.clone()).or_insert(0) += 1;

        // Update cached dependencies for referrer
        if let Some(node) = graph.get_node(&referrer) {
            if let Some(mut cached) = shared_view.get_mut(&referrer) {
                cached.dependencies = node.dependencies.clone().into();
            }
        }
    }

    Ok(())
}

/// Propagate failure to all transitive dependents
pub(super) async fn propagate_failure(
    failed_drv: &DrvId,
    graph: &mut BuildGraph,
    shared_view: &Arc<DashMap<DrvId, CachedNode>>,
    db: &dyn GraphDatabase,
) -> Result<Vec<DrvId>> {
    let blocked = graph.propagate_failure(failed_drv);

    // Update shared view cache for all blocked drvs
    for blocked_id in &blocked {
        if let Some(mut cached) = shared_view.get_mut(blocked_id) {
            cached.build_state = DrvBuildState::TransitiveFailure;
        }
    }

    // Persist transitive failures to database
    if !blocked.is_empty() {
        db.insert_transitive_failures(failed_drv, &blocked)
            .await?;
    }

    Ok(blocked)
}

/// Clear failure and unblock drvs
pub(super) async fn clear_failure(
    formerly_failed: &DrvId,
    graph: &mut BuildGraph,
    shared_view: &Arc<DashMap<DrvId, CachedNode>>,
    db: &dyn GraphDatabase,
) -> Result<Vec<DrvId>> {
    let unblocked = graph.clear_failure(formerly_failed);

    // Update shared view cache for all unblocked drvs
    for unblocked_id in &unblocked {
        if let Some(mut cached) = shared_view.get_mut(unblocked_id) {
            cached.build_state = DrvBuildState::Queued;
        }
    }

    // Persist clearing of transitive failures to database
    db.clear_transitive_failures(formerly_failed)
        .await?;

    Ok(unblocked)
}
