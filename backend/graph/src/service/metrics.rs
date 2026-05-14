// Metrics and eviction monitoring

use std::collections::HashMap;
use std::time::Instant;

use shared::types::{DrvBuildResult, DrvBuildState, DrvId};
use tracing::{debug, error, info};

use crate::eviction::EvictionCandidateSelector;
use crate::graph::BuildGraph;
use crate::traits::GraphMetricsCollector;

/// Update Prometheus metrics based on current graph state
pub(super) fn update_metrics(
    graph: &BuildGraph,
    ref_counts: &HashMap<DrvId, usize>,
    eviction_selector: &EvictionCandidateSelector,
    last_accessed: &HashMap<DrvId, Instant>,
    metrics: Option<&dyn GraphMetricsCollector>,
) {
    let Some(metrics) = metrics else {
        return;
    };

    // Update memory estimate
    let memory_bytes = graph.estimate_memory_bytes();
    metrics.set_memory_bytes(memory_bytes);

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
        let count = graph.get_drvs_by_state(&state).len();
        let state_label = format!("{:?}", state);
        metrics.set_state_count(&state_label, count);
    }

    // Update ref_count histogram
    for (drv_id, &ref_count) in ref_counts {
        let has_dependents = if let Some(node) = graph.get_node(drv_id) {
            !node.dependents.is_empty()
        } else {
            false
        };

        metrics.observe_ref_count(ref_count, has_dependents);
    }

    // Update eviction candidate counts by tier
    let candidate_counts =
        eviction_selector.count_candidates_by_tier(graph, last_accessed, ref_counts);

    let total_candidates: usize = candidate_counts.values().sum();
    metrics.set_eviction_candidates(total_candidates);

    // Update pinned nodes count
    metrics.set_pinned_nodes(graph.pinned_count());

    // Update capacity and utilization
    metrics.set_cache_capacity(graph.capacity());
    metrics.set_cache_utilization(graph.utilization());
}

/// Perform a dry-run eviction check and log what would be evicted
/// This runs periodically to validate eviction policy without actually evicting
/// Also monitors capacity utilization and logs warnings
pub(super) fn maybe_dry_run_eviction_check(
    graph: &BuildGraph,
    last_accessed: &HashMap<DrvId, Instant>,
    ref_counts: &HashMap<DrvId, usize>,
    eviction_selector: &EvictionCandidateSelector,
    last_dry_run_check: &mut Instant,
) {
    let now = Instant::now();
    let since_last_check = now.duration_since(*last_dry_run_check);

    // Run check every 5 minutes
    if since_last_check < std::time::Duration::from_secs(300) {
        return;
    }

    *last_dry_run_check = now;

    // Check capacity utilization
    let current_count = graph.node_count();
    let capacity = graph.capacity();
    let utilization = graph.utilization();
    let pinned_count = graph.pinned_count();

    // Log capacity status
    info!(
        "Cache status: {}/{} nodes ({:.1}% utilized), {} pinned",
        current_count,
        capacity,
        utilization * 100.0,
        pinned_count
    );

    // Warn if capacity is high
    if utilization > 0.90 {
        tracing::warn!(
            "Cache utilization HIGH ({:.1}%): Consider increasing EKA_CI_GRAPH_LRU_CAPACITY \
             (current: {})",
            utilization * 100.0,
            capacity
        );
    } else if utilization > 0.80 {
        tracing::warn!(
            "Cache utilization elevated ({:.1}%): Monitor for potential capacity issues",
            utilization * 100.0
        );
    }

    // Check what would be evicted if we needed to free up 20% of capacity
    let target_evict = current_count / 5; // 20%

    if target_evict == 0 {
        return;
    }

    let candidates =
        eviction_selector.select_candidates(graph, last_accessed, ref_counts, target_evict);

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
            crate::eviction::EvictionTier::Tier1 => tier1_count += 1,
            crate::eviction::EvictionTier::Tier2 => tier2_count += 1,
            crate::eviction::EvictionTier::Tier3 => tier3_count += 1,
        }
    }

    info!(
        "Dry-run eviction check: Would evict {} nodes ({}% of total) - Tier1: {}, Tier2: {}, \
         Tier3: {}",
        candidates.len(),
        (candidates.len() * 100) / current_count,
        tier1_count,
        tier2_count,
        tier3_count
    );

    // Log details of oldest candidate from each tier (for debugging)
    let tier1_oldest = candidates
        .iter()
        .find(|c| c.tier == crate::eviction::EvictionTier::Tier1);
    let tier2_oldest = candidates
        .iter()
        .find(|c| c.tier == crate::eviction::EvictionTier::Tier2);
    let tier3_oldest = candidates
        .iter()
        .find(|c| c.tier == crate::eviction::EvictionTier::Tier3);

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
    let invalid_candidates: Vec<_> = candidates.iter().filter(|c| c.ref_count != 0).collect();

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
