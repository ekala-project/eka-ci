// Traits that define the interfaces the graph service needs from external services

use async_trait::async_trait;
use shared::types::{Drv, DrvBuildState, DrvId};

/// Database operations needed by the graph service
#[async_trait]
pub trait GraphDatabase: Send + Sync {
    /// Get a derivation from the database
    async fn get_drv(&self, drv_path: &DrvId) -> anyhow::Result<Option<Drv>>;

    /// Update the build status of a derivation
    async fn update_drv_status(
        &self,
        drv_id: &DrvId,
        state: &DrvBuildState,
    ) -> anyhow::Result<()>;

    /// Get all derivations (for initial graph load)
    async fn get_all_drvs(&self) -> anyhow::Result<Vec<Drv>>;

    /// Get references for a derivation
    async fn get_drv_refs(&self, drv_id: &DrvId) -> anyhow::Result<Vec<DrvId>>;
}

/// Metrics collection interface
pub trait GraphMetricsCollector: Send + Sync {
    fn set_node_count(&self, count: usize);
    fn set_state_count(&self, state: &str, count: usize);
    fn set_ref_count_stats(&self, min: usize, max: usize, mean: f64);
    fn set_lru_size(&self, size: usize);
    fn set_eviction_candidates(&self, count: usize);
    fn record_command_duration(&self, command_type: &str, duration_seconds: f64);
    fn increment_cache_hits(&self);
    fn increment_cache_misses(&self);
    fn increment_evictions(&self);
}

/// Null implementation for when metrics are disabled
pub struct NullMetrics;

impl GraphMetricsCollector for NullMetrics {
    fn set_node_count(&self, _count: usize) {}
    fn set_state_count(&self, _state: &str, _count: usize) {}
    fn set_ref_count_stats(&self, _min: usize, _max: usize, _mean: f64) {}
    fn set_lru_size(&self, _size: usize) {}
    fn set_eviction_candidates(&self, _count: usize) {}
    fn record_command_duration(&self, _command_type: &str, _duration_seconds: f64) {}
    fn increment_cache_hits(&self) {}
    fn increment_cache_misses(&self) {}
    fn increment_evictions(&self) {}
}
