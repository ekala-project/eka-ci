// Change summary options and configuration loading

use super::{DEFAULT_MAX_PACKAGES_LISTED, impact};

/// Resolved per-call knobs for change summary building.
#[derive(Debug, Clone)]
pub struct ChangeSummaryOptions {
    /// Render the package-changes section.
    pub summary_enabled: bool,
    /// Render the rebuild-impact section.
    pub impact_enabled: bool,
    /// Walk the full transitive dependent set instead of seeds-only.
    pub compute_full_blast_radius: bool,
    /// Surface the "_N packages will rebuild without source changes._" line in markdown.
    pub include_rebuild_only: bool,
    /// Cap on `package_changes` rows surfaced to the renderer.
    pub max_packages_listed: usize,
    /// Cap on `top_blast_radius` rows reported per system.
    pub max_top_blast_radius: usize,
}

impl Default for ChangeSummaryOptions {
    fn default() -> Self {
        Self {
            summary_enabled: true,
            impact_enabled: true,
            compute_full_blast_radius: false,
            include_rebuild_only: true,
            max_packages_listed: DEFAULT_MAX_PACKAGES_LISTED,
            max_top_blast_radius: impact::DEFAULT_MAX_TOP_BLAST_RADIUS,
        }
    }
}

/// Status result from config loading, carrying parse errors when present.
#[derive(Debug, Default, Clone)]
pub struct ConfigLoadStatus {
    /// Set when `.ekaci/config.json` was present but failed to parse.
    /// Contains a user-facing error message suitable for display in banners or logs.
    pub parse_error: Option<String>,
}

// TODO: Metrics support - this is a placeholder
// The actual implementation should be in server or made generic
#[allow(dead_code)]
pub struct ChangeSummaryMetrics {
    pub total_duration_seconds: MetricStub,
    pub metadata_unavailable_total: MetricStub,
    pub truncated_total: MetricStub,
    pub rebuild_impact_seeds: MetricStub,
    pub rebuild_impact_traversal_duration_seconds: MetricStub,
    pub cache_hits_total: MetricStub,
    pub cache_misses_total: MetricStub,
}

#[allow(dead_code)]
pub struct MetricStub;

impl MetricStub {
    pub fn with_label_values(&self, _labels: &[&str]) -> &Self {
        self
    }

    pub fn observe(&self, _value: f64) {}

    pub fn inc(&self) {}
}
