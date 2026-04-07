use std::sync::Arc;

use prometheus::{CounterVec, GaugeVec, Opts, Registry};

/// Metrics for tracking build state across the system
#[derive(Clone)]
pub struct BuildMetrics {
    /// Number of builds currently executing (gauge by platform)
    pub active_builds: GaugeVec,
    /// Number of builds waiting in queue (gauge by platform)
    pub queued_builds: GaugeVec,
}

impl BuildMetrics {
    /// Create new BuildMetrics and register with the provided registry
    pub fn new(registry: &Registry) -> anyhow::Result<Arc<Self>> {
        let active_builds = GaugeVec::new(
            Opts::new("eka_builds_active", "Number of builds currently executing")
                .namespace("eka_ci"),
            &["platform"],
        )?;

        let queued_builds = GaugeVec::new(
            Opts::new("eka_builds_queued", "Number of builds waiting in queue").namespace("eka_ci"),
            &["platform"],
        )?;

        registry.register(Box::new(active_builds.clone()))?;
        registry.register(Box::new(queued_builds.clone()))?;

        Ok(Arc::new(Self {
            active_builds,
            queued_builds,
        }))
    }
}

/// Metrics for tracking GraphService memory and cache behavior
#[derive(Clone)]
pub struct GraphMetrics {
    /// Number of nodes currently in the graph by state
    pub nodes_total: GaugeVec,
    /// Estimated memory usage in bytes
    pub memory_bytes_estimate: prometheus::Gauge,
    /// Cache hits in shared_view lookups
    pub cache_hits_total: CounterVec,
    /// Cache misses in shared_view lookups
    pub cache_misses_total: CounterVec,
    /// Number of eviction candidates by tier
    pub eviction_candidates_total: GaugeVec,
    /// Number of actual evictions performed
    pub evictions_total: CounterVec,
    /// Reference count distribution histogram
    pub ref_count_histogram: prometheus::HistogramVec,
}

impl GraphMetrics {
    /// Create new GraphMetrics and register with the provided registry
    pub fn new(registry: &Registry) -> anyhow::Result<Arc<Self>> {
        let nodes_total = GaugeVec::new(
            Opts::new("graph_nodes_total", "Number of nodes in the graph by state")
                .namespace("eka_ci"),
            &["state"],
        )?;

        let memory_bytes_estimate = prometheus::Gauge::with_opts(
            Opts::new("graph_memory_bytes_estimate", "Estimated memory usage of the graph")
                .namespace("eka_ci"),
        )?;

        let cache_hits_total = CounterVec::new(
            Opts::new("graph_cache_hits_total", "Number of successful cache lookups")
                .namespace("eka_ci"),
            &["operation"],
        )?;

        let cache_misses_total = CounterVec::new(
            Opts::new("graph_cache_misses_total", "Number of cache misses (should be zero)")
                .namespace("eka_ci"),
            &["operation"],
        )?;

        let eviction_candidates_total = GaugeVec::new(
            Opts::new(
                "graph_eviction_candidates_total",
                "Number of nodes eligible for eviction by tier",
            )
            .namespace("eka_ci"),
            &["tier"],
        )?;

        let evictions_total = CounterVec::new(
            Opts::new("graph_evictions_total", "Number of nodes evicted from graph")
                .namespace("eka_ci"),
            &["tier"],
        )?;

        let ref_count_histogram = prometheus::HistogramVec::new(
            prometheus::HistogramOpts::new(
                "graph_ref_count_histogram",
                "Distribution of reference counts",
            )
            .namespace("eka_ci")
            .buckets(vec![0.0, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0]),
            &["has_dependents"],
        )?;

        registry.register(Box::new(nodes_total.clone()))?;
        registry.register(Box::new(memory_bytes_estimate.clone()))?;
        registry.register(Box::new(cache_hits_total.clone()))?;
        registry.register(Box::new(cache_misses_total.clone()))?;
        registry.register(Box::new(eviction_candidates_total.clone()))?;
        registry.register(Box::new(evictions_total.clone()))?;
        registry.register(Box::new(ref_count_histogram.clone()))?;

        Ok(Arc::new(Self {
            nodes_total,
            memory_bytes_estimate,
            cache_hits_total,
            cache_misses_total,
            eviction_candidates_total,
            evictions_total,
            ref_count_histogram,
        }))
    }
}
