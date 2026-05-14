// Implementation of graph::traits::GraphMetricsCollector for GraphMetrics

use graph::traits::GraphMetricsCollector;

use super::GraphMetrics;

impl GraphMetricsCollector for GraphMetrics {
    fn set_node_count(&self, count: usize) {
        // This is called per-state, so we don't have a single total metric
        // The caller will call set_state_count for each state
    }

    fn set_state_count(&self, state: &str, count: usize) {
        self.nodes_total
            .with_label_values(&[state])
            .set(count as f64);
    }

    fn set_ref_count_stats(&self, _min: usize, _max: usize, _mean: f64) {
        // These aggregate stats aren't currently exposed as metrics
        // The ref_count_histogram captures the distribution
    }

    fn set_lru_size(&self, _size: usize) {
        // LRU size isn't currently tracked as a separate metric
        // Cache capacity and utilization serve this purpose
    }

    fn set_eviction_candidates(&self, count: usize) {
        // Total eviction candidates across all tiers
        // Note: The Prometheus metric uses tier labels, so we'd need to track per-tier
        // For now, this is a summary metric that isn't directly exposed
    }

    fn record_command_duration(&self, _command_type: &str, _duration_seconds: f64) {
        // Command duration tracking isn't currently implemented in GraphMetrics
        // This could be added if needed
    }

    fn increment_cache_hits(&self) {
        self.cache_hits_total
            .with_label_values(&["default"])
            .inc();
    }

    fn increment_cache_misses(&self) {
        self.cache_misses_total
            .with_label_values(&["default"])
            .inc();
    }

    fn increment_evictions(&self) {
        self.evictions_total
            .with_label_values(&["default"])
            .inc();
    }

    fn set_memory_bytes(&self, bytes: usize) {
        self.memory_bytes_estimate.set(bytes as f64);
    }

    fn observe_ref_count(&self, count: usize, has_dependents: bool) {
        let label = if has_dependents { "true" } else { "false" };
        self.ref_count_histogram
            .with_label_values(&[label])
            .observe(count as f64);
    }

    fn increment_cache_reloads(&self) {
        self.cache_reloads_total.inc();
    }

    fn observe_cache_reload_duration(&self, seconds: f64) {
        self.cache_reload_duration_seconds.observe(seconds);
    }

    fn set_pinned_nodes(&self, count: usize) {
        self.pinned_nodes_total.set(count as f64);
    }

    fn set_cache_capacity(&self, capacity: usize) {
        self.cache_capacity.set(capacity as f64);
    }

    fn set_cache_utilization(&self, utilization: f64) {
        self.cache_utilization.set(utilization);
    }
}
