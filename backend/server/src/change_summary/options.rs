// Change summary options and configuration loading

use sqlx::{Pool, Sqlite};

use super::{DEFAULT_MAX_PACKAGES_LISTED, impact};
use crate::jobset_data::JobsetData;
use crate::metrics::ChangeSummaryMetrics;

/// Resolved per-call knobs for [`build_change_summary`].
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

impl From<&crate::ci::config::CIConfig> for ChangeSummaryOptions {
    fn from(cfg: &crate::ci::config::CIConfig) -> Self {
        let mut out = Self::default();
        if let Some(pcs) = &cfg.package_change_summary {
            out.summary_enabled = pcs.enabled;
            out.max_packages_listed = pcs.max_packages_listed;
            out.include_rebuild_only = pcs.include_rebuild_only;
        }
        if let Some(ri) = &cfg.rebuild_impact {
            out.impact_enabled = ri.enabled;
            out.max_top_blast_radius = ri.max_top_blast_radius;
            out.compute_full_blast_radius = ri.compute_full_blast_radius;
        }
        out
    }
}

/// Status result from config loading, carrying parse errors when present.
#[derive(Debug, Default, Clone)]
pub struct ConfigLoadStatus {
    /// Set when `.ekaci/config.json` was present but failed to parse.
    /// Contains a user-facing error message suitable for display in banners or logs.
    pub parse_error: Option<String>,
}

/// Resolve options from jobset data's on-disk `.ekaci/config.json` (platform-agnostic).
/// This is the preferred interface for platform services.
pub async fn resolve_options_from_jobset_data(
    jobset_data: &JobsetData,
    metrics: Option<&ChangeSummaryMetrics>,
) -> (ChangeSummaryOptions, ConfigLoadStatus) {
    let load_result = crate::ci::load_repo_ci_config(
        &jobset_data.domain,
        &jobset_data.owner,
        &jobset_data.repo,
        &jobset_data.sha,
    );

    // Increment metric based on outcome
    if let Some(m) = metrics {
        let outcome = match &load_result {
            crate::ci::CIConfigLoad::Loaded(_) => "loaded",
            crate::ci::CIConfigLoad::Absent => "absent",
            crate::ci::CIConfigLoad::Unreadable(_) => "unreadable",
            crate::ci::CIConfigLoad::Invalid { .. } => "invalid",
        };
        m.config_load_total.with_label_values(&[outcome]).inc();
    }

    match load_result {
        crate::ci::CIConfigLoad::Loaded(cfg) => (
            ChangeSummaryOptions::from(&cfg),
            ConfigLoadStatus::default(),
        ),
        crate::ci::CIConfigLoad::Absent => {
            tracing::debug!(
                "No .ekaci/config.json for {}/{}@{}; using defaults",
                jobset_data.owner,
                jobset_data.repo,
                jobset_data.sha
            );
            (ChangeSummaryOptions::default(), ConfigLoadStatus::default())
        },
        crate::ci::CIConfigLoad::Unreadable(e) => {
            tracing::debug!(
                "Unreadable .ekaci/config.json for {}/{}@{}: {}; using defaults",
                jobset_data.owner,
                jobset_data.repo,
                jobset_data.sha,
                e
            );
            (ChangeSummaryOptions::default(), ConfigLoadStatus::default())
        },
        crate::ci::CIConfigLoad::Invalid { source, error } => {
            tracing::warn!(
                "Invalid .ekaci/config.json at {}: {}; using defaults",
                source,
                error
            );
            let parse_error = Some(format!("{}: {}", source, error));
            (
                ChangeSummaryOptions::default(),
                ConfigLoadStatus { parse_error },
            )
        },
    }
}

/// Resolve options from the head jobset's on-disk `.ekaci/config.json`; defaults on any miss.
///
/// Returns options and a status indicating whether a parse error occurred. Only `Invalid`
/// configs produce a status with `parse_error` set; absent and unreadable configs return
/// default status (preserving silent fallback behavior).
///
/// Increments the `config_load_total` metric counter (when metrics are provided) to track
/// config load outcomes for observability and alerting.
///
/// NOTE: This function is GitHub-specific. Platform services should use
/// `resolve_options_from_jobset_data` instead.
pub async fn resolve_options_for_jobset(
    pool: &Pool<Sqlite>,
    head_sha: &str,
    job: &str,
    metrics: Option<&ChangeSummaryMetrics>,
) -> (ChangeSummaryOptions, ConfigLoadStatus) {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT owner, repo_name FROM GitHubJobSets WHERE sha = ? AND job = ?")
            .bind(head_sha)
            .bind(job)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();

    let Some((owner, repo)) = row else {
        tracing::debug!(
            "No jobset for sha={} job={}; using default change-summary options",
            head_sha,
            job
        );
        return (ChangeSummaryOptions::default(), ConfigLoadStatus::default());
    };

    let load_result = crate::ci::load_repo_ci_config("github.com", &owner, &repo, head_sha);

    // Increment metric based on outcome
    if let Some(m) = metrics {
        let outcome = match &load_result {
            crate::ci::CIConfigLoad::Loaded(_) => "loaded",
            crate::ci::CIConfigLoad::Absent => "absent",
            crate::ci::CIConfigLoad::Unreadable(_) => "unreadable",
            crate::ci::CIConfigLoad::Invalid { .. } => "invalid",
        };
        m.config_load_total.with_label_values(&[outcome]).inc();
    }

    match load_result {
        crate::ci::CIConfigLoad::Loaded(cfg) => (
            ChangeSummaryOptions::from(&cfg),
            ConfigLoadStatus::default(),
        ),
        crate::ci::CIConfigLoad::Absent => {
            tracing::debug!(
                "No .ekaci/config.json for {}/{}@{}; using defaults",
                owner,
                repo,
                head_sha
            );
            (ChangeSummaryOptions::default(), ConfigLoadStatus::default())
        },
        crate::ci::CIConfigLoad::Unreadable(e) => {
            tracing::debug!(
                "Unreadable .ekaci/config.json for {}/{}@{}: {}; using defaults",
                owner,
                repo,
                head_sha,
                e
            );
            (ChangeSummaryOptions::default(), ConfigLoadStatus::default())
        },
        crate::ci::CIConfigLoad::Invalid { source, error } => {
            tracing::warn!(
                "Invalid .ekaci/config.json at {}: {}; using defaults",
                source,
                error
            );
            let parse_error = Some(format!("{}: {}", source, error));
            (
                ChangeSummaryOptions::default(),
                ConfigLoadStatus { parse_error },
            )
        },
    }
}
