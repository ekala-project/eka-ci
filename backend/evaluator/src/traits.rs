// Traits that define the interfaces the eval service needs from external services

use async_trait::async_trait;
use shared::types::{Drv, DrvId};

/// Database operations needed by the eval service
#[async_trait]
pub trait EvalDatabase: Send + Sync {
    /// Get a derivation from the database
    async fn get_drv(&self, drv_path: &DrvId) -> anyhow::Result<Option<Drv>>;

    /// Insert derivations and their reference relationships
    async fn insert_drvs_and_references(
        &self,
        drvs: &[Drv],
        refs: &[(DrvId, DrvId)],
    ) -> anyhow::Result<()>;
}

/// Metrics collection interface for nix-eval-jobs operations
pub trait EvalMetricsCollector: Send + Sync {
    /// Increment the total items counter (labeled by type: "drv" or "error")
    fn items_total_inc(&self, label: &str, count: u64);

    /// Increment the truncated total counter (labeled by reason)
    fn truncated_total_inc(&self, reason: &str);

    /// Observe the number of output entries (drvs + errors)
    fn output_entries_observe(&self, count: f64);

    /// Observe the number of bytes read from nix-eval-jobs
    fn output_bytes_observe(&self, bytes: f64);
}

/// Null implementation for when metrics are disabled
pub struct NullMetrics;

impl EvalMetricsCollector for NullMetrics {
    fn items_total_inc(&self, _label: &str, _count: u64) {}

    fn truncated_total_inc(&self, _reason: &str) {}

    fn output_entries_observe(&self, _count: f64) {}

    fn output_bytes_observe(&self, _bytes: f64) {}
}
