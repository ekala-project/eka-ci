// Package change and change summary builder functions

use anyhow::Context;
use sqlx::{Pool, Sqlite};

use super::options::{ChangeSummaryOptions, ConfigLoadStatus};
use super::types::{ChangeSummary, ChangeSummaryRebuildImpact, PackageChangesResponse};
use super::{classify, impact, render};
use crate::jobset_data::JobsetData;
use crate::metrics::ChangeSummaryMetrics;

/// Build package changes response from jobset IDs (platform-agnostic).
/// This is the preferred interface for platform services that have already
/// resolved their jobset IDs from platform-specific tables.
pub async fn build_package_changes_from_jobset_ids(
    pool: &Pool<Sqlite>,
    head_jobset_id: i64,
    base_jobset_id: Option<i64>,
    head_jobset_data: &JobsetData,
    max_packages_listed: usize,
) -> anyhow::Result<PackageChangesResponse> {
    let head_rows = classify::load_job_drv_rows(pool, head_jobset_id).await?;
    let base_rows = match base_jobset_id {
        Some(id) => classify::load_job_drv_rows(pool, id).await?,
        None => Vec::new(),
    };

    let (mut changes, metadata_available) =
        classify::compute_package_changes(&head_rows, &base_rows);

    let truncated = changes.len() > max_packages_listed;
    if truncated {
        changes.truncate(max_packages_listed);
    }

    let computed_at = chrono::Utc::now().to_rfc3339();

    Ok(PackageChangesResponse {
        head_sha: head_jobset_data.sha.clone(),
        base_sha: head_jobset_data.sha.clone(), // Will be updated by caller if base exists
        job: head_jobset_data.job.clone(),
        computed_at,
        metadata_available,
        package_changes: changes,
        truncated,
    })
}

/// Resolve `(head_sha, base_sha, job)` to two `Job ⋈ Drv` row sets and
/// classify them into a [`PackageChangesResponse`]. `Ok(None)` when the
/// head jobset is missing (caller maps to 404); a missing base jobset
/// classifies every head row as `Added`.
///
/// NOTE: This function is GitHub-specific. Platform services should use
/// `build_package_changes_from_jobset_ids` instead.
pub async fn build_package_changes_response(
    pool: &Pool<Sqlite>,
    head_sha: &str,
    base_sha: &str,
    job: &str,
    max_packages_listed: usize,
) -> anyhow::Result<Option<PackageChangesResponse>> {
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

    let base_jobset_id: Option<i64> =
        sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
            .bind(base_sha)
            .bind(job)
            .fetch_optional(pool)
            .await
            .context("Failed to resolve base jobset id")?;

    let head_rows = classify::load_job_drv_rows(pool, head_jobset_id).await?;
    let base_rows = match base_jobset_id {
        Some(id) => classify::load_job_drv_rows(pool, id).await?,
        None => Vec::new(),
    };

    let (mut changes, metadata_available) =
        classify::compute_package_changes(&head_rows, &base_rows);

    let truncated = changes.len() > max_packages_listed;
    if truncated {
        changes.truncate(max_packages_listed);
    }

    let computed_at = chrono::Utc::now().to_rfc3339();

    Ok(Some(PackageChangesResponse {
        head_sha: head_sha.to_string(),
        base_sha: base_sha.to_string(),
        job: job.to_string(),
        computed_at,
        metadata_available,
        package_changes: changes,
        truncated,
    }))
}

/// Build change summary from jobset IDs (platform-agnostic).
/// This is the preferred interface for platform services.
pub async fn build_change_summary_from_jobset_ids(
    pool: &Pool<Sqlite>,
    graph: &crate::graph::GraphServiceHandle,
    head_jobset_id: i64,
    base_jobset_id: Option<i64>,
    head_jobset_data: &JobsetData,
    base_sha: &str,
    options: &ChangeSummaryOptions,
    status: &ConfigLoadStatus,
    metrics: Option<&ChangeSummaryMetrics>,
) -> anyhow::Result<ChangeSummary> {
    let end_to_end_start = std::time::Instant::now();

    let classify_start = std::time::Instant::now();
    let mut pkg_resp = if options.summary_enabled {
        build_package_changes_from_jobset_ids(
            pool,
            head_jobset_id,
            base_jobset_id,
            head_jobset_data,
            options.max_packages_listed,
        )
        .await?
    } else {
        PackageChangesResponse {
            head_sha: head_jobset_data.sha.clone(),
            base_sha: base_sha.to_string(),
            job: head_jobset_data.job.clone(),
            computed_at: chrono::Utc::now().to_rfc3339(),
            metadata_available: true,
            package_changes: Vec::new(),
            truncated: false,
        }
    };
    // Update base_sha from parameter
    pkg_resp.base_sha = base_sha.to_string();

    if let Some(m) = metrics {
        m.total_duration_seconds
            .with_label_values(&["classify"])
            .observe(classify_start.elapsed().as_secs_f64());
    }

    if !pkg_resp.metadata_available {
        if let Some(m) = metrics {
            m.metadata_unavailable_total.inc();
        }
    }

    // None on a race with a jobset delete; treat as "no impact" so the package-change view stays.
    let impact_start = std::time::Instant::now();
    let impact_resp = if options.impact_enabled {
        impact::build_rebuild_impact_response_cached(
            pool,
            graph,
            &head_jobset_data.sha,
            base_sha,
            &head_jobset_data.job,
            options.max_top_blast_radius,
            options.compute_full_blast_radius,
            metrics,
        )
        .await?
    } else {
        None
    };
    if let Some(m) = metrics {
        m.total_duration_seconds
            .with_label_values(&["impact"])
            .observe(impact_start.elapsed().as_secs_f64());
    }

    let (per_system, total_unique_drvs, impact_computed_at) = match impact_resp {
        Some(r) => (r.per_system, r.total_unique_drvs, Some(r.computed_at)),
        None => (Vec::new(), 0, None),
    };

    // Prefer impact's `computed_at` (cache key); fall back to the package-change timestamp.
    let computed_at = impact_computed_at.unwrap_or(pkg_resp.computed_at);

    let mut summary = ChangeSummary {
        head_sha: pkg_resp.head_sha,
        base_sha: pkg_resp.base_sha,
        job: pkg_resp.job,
        computed_at,
        metadata_available: pkg_resp.metadata_available,
        package_changes: pkg_resp.package_changes,
        rebuild_impact: ChangeSummaryRebuildImpact {
            per_system,
            total_unique_drvs,
        },
        truncated: pkg_resp.truncated,
        config_load_error: status.parse_error.clone(),
        markdown: String::new(),
    };

    let render_start = std::time::Instant::now();
    let render_opts = render::RenderOptions {
        include_rebuild_only: options.include_rebuild_only,
        config_load_error: status.parse_error.clone(),
    };
    let (markdown, render_truncation) = render::render(&summary, &render_opts);
    if let Some(m) = metrics {
        m.total_duration_seconds
            .with_label_values(&["render"])
            .observe(render_start.elapsed().as_secs_f64());
    }
    summary.markdown = markdown;
    if render_truncation.any() {
        summary.truncated = true;
    }

    if let Some(m) = metrics {
        if render_truncation.dropped_maintainers
            || render_truncation.dropped_license
            || render_truncation.dropped_rebuild_only
        {
            m.truncated_total.with_label_values(&["columns"]).inc();
        }
        if render_truncation.collapsed_to_counts {
            m.truncated_total.with_label_values(&["summary"]).inc();
        }
        m.total_duration_seconds
            .with_label_values(&["end_to_end"])
            .observe(end_to_end_start.elapsed().as_secs_f64());
    }

    Ok(summary)
}

/// Compose classify + (cached) impact + render into a full [`ChangeSummary`].
/// `Ok(None)` when the head jobset is missing. Structured `package_changes`
/// is never truncated here; markdown truncation is reported on the returned
/// summary and reflected in the rendered footer.
///
/// The `status` parameter carries config parse error information which is
/// surfaced to users via both the JSON response and the rendered markdown banner.
///
/// NOTE: This function is GitHub-specific. Platform services should use
/// `build_change_summary_from_jobset_ids` instead.
pub async fn build_change_summary(
    pool: &Pool<Sqlite>,
    graph: &crate::graph::GraphServiceHandle,
    head_sha: &str,
    base_sha: &str,
    job: &str,
    options: &ChangeSummaryOptions,
    status: &ConfigLoadStatus,
    metrics: Option<&ChangeSummaryMetrics>,
) -> anyhow::Result<Option<ChangeSummary>> {
    let end_to_end_start = std::time::Instant::now();

    // Head jobset must exist either way — drives the 404.
    let head_jobset_id: Option<i64> =
        sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
            .bind(head_sha)
            .bind(job)
            .fetch_optional(pool)
            .await
            .context("Failed to resolve head jobset id")?;
    if head_jobset_id.is_none() {
        return Ok(None);
    }

    let classify_start = std::time::Instant::now();
    let pkg_resp = if options.summary_enabled {
        build_package_changes_response(pool, head_sha, base_sha, job, options.max_packages_listed)
            .await?
    } else {
        Some(PackageChangesResponse {
            head_sha: head_sha.to_string(),
            base_sha: base_sha.to_string(),
            job: job.to_string(),
            computed_at: chrono::Utc::now().to_rfc3339(),
            metadata_available: true,
            package_changes: Vec::new(),
            truncated: false,
        })
    };
    if let Some(m) = metrics {
        m.total_duration_seconds
            .with_label_values(&["classify"])
            .observe(classify_start.elapsed().as_secs_f64());
    }
    let Some(pkg_resp) = pkg_resp else {
        return Ok(None);
    };

    if !pkg_resp.metadata_available {
        if let Some(m) = metrics {
            m.metadata_unavailable_total.inc();
        }
    }

    // None on a race with a jobset delete; treat as "no impact" so the package-change view stays.
    let impact_start = std::time::Instant::now();
    let impact_resp = if options.impact_enabled {
        impact::build_rebuild_impact_response_cached(
            pool,
            graph,
            head_sha,
            base_sha,
            job,
            options.max_top_blast_radius,
            options.compute_full_blast_radius,
            metrics,
        )
        .await?
    } else {
        None
    };
    if let Some(m) = metrics {
        m.total_duration_seconds
            .with_label_values(&["impact"])
            .observe(impact_start.elapsed().as_secs_f64());
    }

    let (per_system, total_unique_drvs, impact_computed_at) = match impact_resp {
        Some(r) => (r.per_system, r.total_unique_drvs, Some(r.computed_at)),
        None => (Vec::new(), 0, None),
    };

    // Prefer impact's `computed_at` (cache key); fall back to the package-change timestamp.
    let computed_at = impact_computed_at.unwrap_or(pkg_resp.computed_at);

    let mut summary = ChangeSummary {
        head_sha: pkg_resp.head_sha,
        base_sha: pkg_resp.base_sha,
        job: pkg_resp.job,
        computed_at,
        metadata_available: pkg_resp.metadata_available,
        package_changes: pkg_resp.package_changes,
        rebuild_impact: ChangeSummaryRebuildImpact {
            per_system,
            total_unique_drvs,
        },
        truncated: pkg_resp.truncated,
        config_load_error: status.parse_error.clone(),
        markdown: String::new(),
    };

    let render_start = std::time::Instant::now();
    let render_opts = render::RenderOptions {
        include_rebuild_only: options.include_rebuild_only,
        config_load_error: status.parse_error.clone(),
    };
    let (markdown, render_truncation) = render::render(&summary, &render_opts);
    if let Some(m) = metrics {
        m.total_duration_seconds
            .with_label_values(&["render"])
            .observe(render_start.elapsed().as_secs_f64());
    }
    summary.markdown = markdown;
    if render_truncation.any() {
        summary.truncated = true;
    }

    if let Some(m) = metrics {
        if render_truncation.dropped_maintainers
            || render_truncation.dropped_license
            || render_truncation.dropped_rebuild_only
        {
            m.truncated_total.with_label_values(&["columns"]).inc();
        }
        if render_truncation.collapsed_to_counts {
            m.truncated_total.with_label_values(&["summary"]).inc();
        }
        m.total_duration_seconds
            .with_label_values(&["end_to_end"])
            .observe(end_to_end_start.elapsed().as_secs_f64());
    }

    Ok(Some(summary))
}
