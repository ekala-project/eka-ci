//! Compatibility layer for change_summary crate
//!
//! This module provides:
//! - Legacy GitHub-specific functions that query GitHubJobSets
//! - Config resolution functions that bridge server's CI config with change_summary
//! - Metrics handling that converts between server and change_summary types

use anyhow::Context;
use sqlx::{Pool, Sqlite};

use crate::ci::{self, config::CIConfig};
use crate::graph::GraphServiceHandle;
use crate::metrics::ChangeSummaryMetrics;
use shared::types::JobsetData;

// Re-export change_summary types for convenience
pub use change_summary::{
    types::PackageChangesResponse,
    ChangeSummary, ChangeSummaryOptions, ChangeSummaryRebuildImpact, ConfigLoadStatus,
    PackageChange, PerSystemImpact, RebuildImpactResponse, TopBlastRadiusEntry,
    DEFAULT_MAX_PACKAGES_LISTED,
};

// Re-export impact module functions
pub use change_summary::impact;
pub use change_summary::impact::DEFAULT_MAX_TOP_BLAST_RADIUS;

/// Build package changes response from head/base SHA and job (GitHub-specific).
///
/// This is a legacy function that queries GitHubJobSets. New code should use
/// `change_summary::build_package_changes_from_jobset_ids` directly.
pub async fn build_package_changes_response(
    pool: &Pool<Sqlite>,
    head_sha: &str,
    base_sha: &str,
    job: &str,
    max_packages_listed: usize,
) -> anyhow::Result<Option<change_summary::types::PackageChangesResponse>> {
    // Resolve head jobset ID
    let head_jobset_id: Option<i64> =
        sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
            .bind(head_sha)
            .bind(job)
            .fetch_optional(pool)
            .await
            .context("Failed to resolve head jobset id")?;

    let Some(head_jobset_id) = head_jobset_id else {
        return Ok(None);
    };

    // Resolve base jobset ID (optional)
    let base_jobset_id: Option<i64> =
        sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
            .bind(base_sha)
            .bind(job)
            .fetch_optional(pool)
            .await
            .context("Failed to resolve base jobset id")?;

    // Get jobset metadata for the head
    let (owner, repo, domain): (String, String, String) = sqlx::query_as(
        "SELECT owner, repo_name, 'github.com' FROM GitHubJobSets WHERE ROWID = ?",
    )
    .bind(head_jobset_id)
    .fetch_one(pool)
    .await
    .context("Failed to fetch head jobset metadata")?;

    let jobset_data = JobsetData::new(owner, repo, domain, head_sha, job, None);

    // Use the platform-agnostic function
    Ok(Some(
        change_summary::build_package_changes_from_jobset_ids(
            pool,
            head_jobset_id,
            base_jobset_id,
            &jobset_data,
            max_packages_listed,
        )
        .await?,
    ))
}

/// Build full change summary from head/base SHA and job (GitHub-specific).
///
/// This is a legacy function that queries GitHubJobSets. New code should use
/// `change_summary::build_change_summary_from_jobset_ids` directly.
pub async fn build_change_summary(
    pool: &Pool<Sqlite>,
    graph: &GraphServiceHandle,
    head_sha: &str,
    base_sha: &str,
    job: &str,
    opts: &ChangeSummaryOptions,
    status: &ConfigLoadStatus,
    metrics: Option<&ChangeSummaryMetrics>,
) -> anyhow::Result<Option<ChangeSummary>> {
    // Resolve head jobset ID
    let head_jobset_id: Option<i64> =
        sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
            .bind(head_sha)
            .bind(job)
            .fetch_optional(pool)
            .await
            .context("Failed to resolve head jobset id")?;

    let Some(head_jobset_id) = head_jobset_id else {
        return Ok(None);
    };

    // Resolve base jobset ID (optional)
    let base_jobset_id: Option<i64> =
        sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
            .bind(base_sha)
            .bind(job)
            .fetch_optional(pool)
            .await
            .context("Failed to resolve base jobset id")?;

    // Get jobset metadata for the head
    let (owner, repo, domain): (String, String, String) = sqlx::query_as(
        "SELECT owner, repo_name, 'github.com' FROM GitHubJobSets WHERE ROWID = ?",
    )
    .bind(head_jobset_id)
    .fetch_one(pool)
    .await
    .context("Failed to fetch head jobset metadata")?;

    let jobset_data = JobsetData::new(owner, repo, domain, head_sha, job, None);

    // Note: metrics parameter is ignored since change_summary has stub metrics
    // In the future, we may pass real metrics when the crate supports it
    let _ = metrics;

    // Use the platform-agnostic function
    let summary = change_summary::build_change_summary_from_jobset_ids(
        pool,
        graph,
        head_jobset_id,
        base_jobset_id,
        &jobset_data,
        base_sha,
        opts,
        status,
        None, // TODO: Pass actual metrics when change_summary supports it
    )
    .await?;

    Ok(Some(summary))
}

/// Resolve options from the head jobset's on-disk `.ekaci/config.json` (GitHub-specific).
///
/// Returns options and a status indicating whether a parse error occurred.
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
        return (
            ChangeSummaryOptions::default(),
            ConfigLoadStatus::default(),
        );
    };

    let jobset_data = JobsetData::new(owner, repo, "github.com", head_sha, job, None);
    resolve_options_from_jobset_data(&jobset_data, metrics).await
}

/// Resolve options from jobset data's on-disk `.ekaci/config.json` (platform-agnostic).
///
/// This is the preferred interface for platform services.
pub async fn resolve_options_from_jobset_data(
    jobset_data: &JobsetData,
    metrics: Option<&ChangeSummaryMetrics>,
) -> (ChangeSummaryOptions, ConfigLoadStatus) {
    let load_result = ci::load_repo_ci_config(
        &jobset_data.domain,
        &jobset_data.owner,
        &jobset_data.repo,
        &jobset_data.sha,
    );

    // Increment metric based on outcome
    if let Some(m) = metrics {
        let outcome = match &load_result {
            ci::CIConfigLoad::Loaded(_) => "loaded",
            ci::CIConfigLoad::Absent => "absent",
            ci::CIConfigLoad::Unreadable(_) => "unreadable",
            ci::CIConfigLoad::Invalid { .. } => "invalid",
        };
        m.config_load_total.with_label_values(&[outcome]).inc();
    }

    match load_result {
        ci::CIConfigLoad::Loaded(cfg) => (
            ChangeSummaryOptions::from(&cfg),
            ConfigLoadStatus::default(),
        ),
        ci::CIConfigLoad::Absent => {
            tracing::debug!(
                "No .ekaci/config.json for {}/{}@{}; using defaults",
                jobset_data.owner,
                jobset_data.repo,
                jobset_data.sha
            );
            (
                ChangeSummaryOptions::default(),
                ConfigLoadStatus::default(),
            )
        }
        ci::CIConfigLoad::Unreadable(e) => {
            tracing::debug!(
                "Unreadable .ekaci/config.json for {}/{}@{}: {}; using defaults",
                jobset_data.owner,
                jobset_data.repo,
                jobset_data.sha,
                e
            );
            (
                ChangeSummaryOptions::default(),
                ConfigLoadStatus::default(),
            )
        }
        ci::CIConfigLoad::Invalid { source, error } => {
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
        }
    }
}

impl From<&CIConfig> for ChangeSummaryOptions {
    fn from(cfg: &CIConfig) -> Self {
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
