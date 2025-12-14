use std::sync::Arc;

use prometheus::{GaugeVec, Opts, Registry};

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
