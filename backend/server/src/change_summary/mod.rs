//! Package change summary and rebuild impact analysis.
//!
//! Composes a per-PR view from the structured package diff ([`classify`]),
//! per-system rebuild + blast-radius numbers ([`impact`]), and a markdown
//! rendering ([`render`]) suitable for posting as a GitHub check.

pub mod cache;
pub mod classify;
pub mod impact;
pub mod render;
pub mod types;

use anyhow::Context;
use sqlx::{Pool, Sqlite};
#[allow(unused_imports)]
pub use types::{
    ChangeSummary, ChangeSummaryRebuildImpact, PackageChange, PackageChangesResponse,
    PerSystemImpact, RebuildImpactResponse, TopBlastRadiusEntry,
};

use crate::metrics::ChangeSummaryMetrics;

/// Resolve `(head_sha, base_sha, job)` to two `Job ⋈ Drv` row sets and
/// classify them into a [`PackageChangesResponse`]. `Ok(None)` when the
/// head jobset is missing (caller maps to 404); a missing base jobset
/// classifies every head row as `Added`.
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

/// Default cap on package-change rows surfaced to the renderer.
pub const DEFAULT_MAX_PACKAGES_LISTED: usize = 100;

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

/// Resolve options from the head jobset's on-disk `.ekaci/config.json`; defaults on any miss.
pub async fn resolve_options_for_jobset(
    pool: &Pool<Sqlite>,
    head_sha: &str,
    job: &str,
) -> ChangeSummaryOptions {
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
        return ChangeSummaryOptions::default();
    };

    match crate::ci::load_repo_ci_config("github.com", &owner, &repo, head_sha) {
        Some(cfg) => ChangeSummaryOptions::from(&cfg),
        None => {
            tracing::debug!(
                "No .ekaci/config.json for {}/{}@{}; using defaults",
                owner,
                repo,
                head_sha
            );
            ChangeSummaryOptions::default()
        },
    }
}

/// Compose classify + (cached) impact + render into a full [`ChangeSummary`].
/// `Ok(None)` when the head jobset is missing. Structured `package_changes`
/// is never truncated here; markdown truncation is reported on the returned
/// summary and reflected in the rendered footer.
pub async fn build_change_summary(
    pool: &Pool<Sqlite>,
    graph: &crate::graph::GraphServiceHandle,
    head_sha: &str,
    base_sha: &str,
    job: &str,
    options: &ChangeSummaryOptions,
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
        markdown: String::new(),
    };

    let render_start = std::time::Instant::now();
    let render_opts = render::RenderOptions {
        include_rebuild_only: options.include_rebuild_only,
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

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::SqlitePool;

    use super::*;
    use crate::db::github::{create_jobs_for_jobset, create_jobset};
    use crate::db::model::DrvId;
    use crate::db::model::build_event::DrvBuildState;
    use crate::db::model::drv::{Drv, insert_drv};
    use crate::nix::nix_eval_jobs::NixEvalDrv;

    /// Build a [`NixEvalDrv`] sufficient to satisfy `create_jobs_for_jobset`.
    /// This is the *evaluation-shape* counterpart of the DB-shape `Drv`
    /// inserted by `make_db_drv`.
    fn make_eval(attr: &str, drv_path: &str, name: &str) -> NixEvalDrv {
        NixEvalDrv {
            attr: attr.to_string(),
            attr_path: vec![attr.to_string()],
            drv_path: drv_path.to_string(),
            input_drvs: None,
            name: name.to_string(),
            outputs: std::collections::HashMap::new(),
            system: "x86_64-linux".to_string(),
            meta: None,
        }
    }

    /// Build a `Drv` row with the package metadata fields populated.
    fn make_db_drv(
        drv_path: &str,
        pname: Option<&str>,
        version: Option<&str>,
        license_json: Option<&str>,
        maintainers_json: Option<&str>,
    ) -> Drv {
        Drv {
            drv_path: DrvId::from_str(drv_path).unwrap(),
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: pname.map(str::to_string),
            version: version.map(str::to_string),
            license_json: license_json.map(str::to_string),
            maintainers_json: maintainers_json.map(str::to_string),
            meta_position: None,
            broken: None,
            insecure: None,
        }
    }

    /// 32-char store hash placeholders that satisfy `DrvId` validation.
    const H_BASE: &str = "1111111111111111111111111111111";
    const H_HEAD: &str = "2222222222222222222222222222222";

    fn drv_path(hash_prefix: &str, name: &str) -> String {
        // Pad the variable hash to a 32-char store hash so DrvId accepts it.
        format!(
            "/nix/store/{hash_prefix}{filler}-{name}.drv",
            // 32 alphanumeric chars total: prefix is 31 → pad with one.
            filler = "z"
        )
    }

    /// Set up a (sha, job) jobset with the supplied (eval_drv, db_drv) rows.
    async fn make_jobset(
        pool: &SqlitePool,
        sha: &str,
        job: &str,
        rows: &[(NixEvalDrv, Drv)],
    ) -> i64 {
        let jobset_id = create_jobset(sha, job, "owner", "repo", None, pool)
            .await
            .expect("create_jobset failed");
        for (_eval, db_drv) in rows {
            insert_drv(pool, db_drv).await.expect("insert_drv failed");
        }
        let evals: Vec<NixEvalDrv> = rows.iter().map(|(e, _)| e.clone()).collect();
        create_jobs_for_jobset(jobset_id, &evals, pool)
            .await
            .expect("create_jobs_for_jobset failed");
        jobset_id
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_response_returns_none_when_head_jobset_missing(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let resp =
            build_package_changes_response(&pool, "missing-sha", "base-sha", "ci", 100).await?;
        assert!(resp.is_none());
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_response_emits_added_when_base_jobset_missing(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let head_drv_path = drv_path(H_HEAD, "hello-2.13");
        let head_eval = make_eval("hello", &head_drv_path, "hello-2.13");
        let head_db = make_db_drv(&head_drv_path, Some("hello"), Some("2.13"), None, None);

        make_jobset(&pool, "head-sha", "ci", &[(head_eval, head_db)]).await;

        let resp = build_package_changes_response(&pool, "head-sha", "missing-base", "ci", 100)
            .await?
            .expect("expected Some response");

        assert_eq!(resp.head_sha, "head-sha");
        assert_eq!(resp.package_changes.len(), 1);
        assert!(matches!(
            resp.package_changes[0],
            PackageChange::Added { .. }
        ));
        assert!(resp.metadata_available);
        assert!(!resp.truncated);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_response_classifies_version_bump_end_to_end(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let base_path = drv_path(H_BASE, "hello-2.12");
        let head_path = drv_path(H_HEAD, "hello-2.13");

        let base_eval = make_eval("hello", &base_path, "hello-2.12");
        let base_db = make_db_drv(&base_path, Some("hello"), Some("2.12"), None, None);
        make_jobset(&pool, "base-sha", "ci", &[(base_eval, base_db)]).await;

        let head_eval = make_eval("hello", &head_path, "hello-2.13");
        let head_db = make_db_drv(&head_path, Some("hello"), Some("2.13"), None, None);
        make_jobset(&pool, "head-sha", "ci", &[(head_eval, head_db)]).await;

        let resp = build_package_changes_response(&pool, "head-sha", "base-sha", "ci", 100)
            .await?
            .expect("expected Some response");

        assert_eq!(resp.package_changes.len(), 1);
        match &resp.package_changes[0] {
            PackageChange::VersionBump {
                pname, old, new, ..
            } => {
                assert_eq!(pname, "hello");
                assert_eq!(old, "2.12");
                assert_eq!(new, "2.13");
            },
            other => panic!("expected VersionBump, got {other:?}"),
        }
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_response_truncates_when_over_limit(pool: SqlitePool) -> anyhow::Result<()> {
        // Five "Added" entries; truncate at 2.
        let mut rows = Vec::new();
        for i in 0..5 {
            // Use a fresh, valid 32-char hash for each row by appending a
            // changing suffix and re-padding.
            let hash = format!("{:0>32}", format!("a{i}"));
            let path = format!("/nix/store/{hash}-pkg{i}-1.0.drv");
            let eval = make_eval(&format!("pkg{i}"), &path, &format!("pkg{i}-1.0"));
            let db = make_db_drv(&path, Some(&format!("pkg{i}")), Some("1.0"), None, None);
            rows.push((eval, db));
        }
        make_jobset(&pool, "head-sha", "ci", &rows).await;

        let resp = build_package_changes_response(&pool, "head-sha", "base-sha", "ci", 2)
            .await?
            .expect("expected Some response");

        assert_eq!(resp.package_changes.len(), 2);
        assert!(resp.truncated);
        Ok(())
    }

    // End-to-end orchestrator tests: classify + impact + render.
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use crate::db::DbService;
    use crate::graph::{GraphCommand, GraphService};

    async fn spawn_graph(pool: &SqlitePool) -> crate::graph::GraphServiceHandle {
        let db_service = DbService { pool: pool.clone() };
        let (tx, rx) = mpsc::channel::<GraphCommand>(64);
        let service = GraphService::new(db_service, rx, None, 1_000_000)
            .await
            .expect("GraphService::new failed");
        let handle = service.handle(tx);
        let cancel = CancellationToken::new();
        tokio::spawn(async move {
            service.run(cancel).await;
        });
        handle
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_summary_returns_none_when_head_jobset_missing(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let graph = spawn_graph(&pool).await;
        let opts = ChangeSummaryOptions::default();
        let summary =
            build_change_summary(&pool, &graph, "missing-sha", "base-sha", "ci", &opts, None)
                .await?;
        assert!(summary.is_none());
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_summary_renders_markdown_with_added_package(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        // Just a head jobset with one Added package; base missing.
        let head_path = drv_path(H_HEAD, "hello-2.13");
        let head_eval = make_eval("hello", &head_path, "hello-2.13");
        let head_db = make_db_drv(&head_path, Some("hello"), Some("2.13"), None, None);
        make_jobset(&pool, "head-sha", "ci", &[(head_eval, head_db)]).await;

        let graph = spawn_graph(&pool).await;
        let opts = ChangeSummaryOptions::default();
        let summary =
            build_change_summary(&pool, &graph, "head-sha", "missing-base", "ci", &opts, None)
                .await?
                .expect("expected Some summary");

        assert_eq!(summary.package_changes.len(), 1);
        assert!(matches!(
            summary.package_changes[0],
            PackageChange::Added { .. }
        ));
        // Markdown should contain headline + the Added row.
        assert!(summary.markdown.contains("## Change summary for"));
        assert!(summary.markdown.contains("### Packages changed (1)"));
        assert!(summary.markdown.contains("| Added | `hello` | 2.13 |"));
        // Rebuild-impact section is rendered even with empty data — but
        // since the head jobset has 1 New job, impact should populate it.
        assert!(summary.markdown.contains("### Rebuild impact"));
        // metadata_available should propagate.
        assert!(summary.metadata_available);
        // Both jobsets present in DB → summary is small → no truncation.
        assert!(!summary.truncated);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_summary_propagates_classify_truncation_flag(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        // 5 head packages, ask for max 2 listed.
        let mut rows = Vec::new();
        for i in 0..5 {
            let hash = format!("{:0>32}", format!("a{i}"));
            let path = format!("/nix/store/{hash}-pkg{i}-1.0.drv");
            let eval = make_eval(&format!("pkg{i}"), &path, &format!("pkg{i}-1.0"));
            let db = make_db_drv(&path, Some(&format!("pkg{i}")), Some("1.0"), None, None);
            rows.push((eval, db));
        }
        make_jobset(&pool, "head-sha", "ci", &rows).await;

        let graph = spawn_graph(&pool).await;
        let opts = ChangeSummaryOptions {
            max_packages_listed: 2,
            ..ChangeSummaryOptions::default()
        };
        let summary =
            build_change_summary(&pool, &graph, "head-sha", "missing-base", "ci", &opts, None)
                .await?
                .expect("expected Some summary");

        assert_eq!(summary.package_changes.len(), 2);
        // classify::truncated → orchestrator surfaces it.
        assert!(summary.truncated);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_summary_skips_classify_when_summary_disabled(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let head_path = drv_path(H_HEAD, "hello-2.13");
        let head_eval = make_eval("hello", &head_path, "hello-2.13");
        let head_db = make_db_drv(&head_path, Some("hello"), Some("2.13"), None, None);
        make_jobset(&pool, "head-sha", "ci", &[(head_eval, head_db)]).await;

        let graph = spawn_graph(&pool).await;
        let opts = ChangeSummaryOptions {
            summary_enabled: false,
            ..ChangeSummaryOptions::default()
        };
        let summary =
            build_change_summary(&pool, &graph, "head-sha", "missing-base", "ci", &opts, None)
                .await?
                .expect("expected Some summary");

        assert!(summary.package_changes.is_empty());
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_summary_skips_impact_when_impact_disabled(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let head_path = drv_path(H_HEAD, "hello-2.13");
        let head_eval = make_eval("hello", &head_path, "hello-2.13");
        let head_db = make_db_drv(&head_path, Some("hello"), Some("2.13"), None, None);
        make_jobset(&pool, "head-sha", "ci", &[(head_eval, head_db)]).await;

        let graph = spawn_graph(&pool).await;
        let opts = ChangeSummaryOptions {
            impact_enabled: false,
            ..ChangeSummaryOptions::default()
        };
        let summary =
            build_change_summary(&pool, &graph, "head-sha", "missing-base", "ci", &opts, None)
                .await?
                .expect("expected Some summary");

        // Package changes still computed; impact section empty.
        assert_eq!(summary.package_changes.len(), 1);
        assert!(summary.rebuild_impact.per_system.is_empty());
        assert_eq!(summary.rebuild_impact.total_unique_drvs, 0);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_summary_returns_none_when_head_missing_even_if_disabled(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let graph = spawn_graph(&pool).await;
        let opts = ChangeSummaryOptions {
            summary_enabled: false,
            impact_enabled: false,
            ..ChangeSummaryOptions::default()
        };
        let summary = build_change_summary(
            &pool,
            &graph,
            "missing-sha",
            "missing-base",
            "ci",
            &opts,
            None,
        )
        .await?;
        assert!(summary.is_none());
        Ok(())
    }

    #[test]
    fn options_from_ci_config_copies_all_knobs() {
        use crate::ci::config::{CIConfig, PackageChangeSummaryConfig, RebuildImpactConfig};

        let cfg = CIConfig {
            jobs: Default::default(),
            checks: Default::default(),
            flake: None,
            package_change_summary: Some(PackageChangeSummaryConfig {
                enabled: false,
                max_packages_listed: 7,
                include_rebuild_only: true,
            }),
            rebuild_impact: Some(RebuildImpactConfig {
                enabled: false,
                max_top_blast_radius: 11,
                compute_full_blast_radius: true,
            }),
        };

        let opts = ChangeSummaryOptions::from(&cfg);
        assert!(!opts.summary_enabled);
        assert!(!opts.impact_enabled);
        assert!(opts.compute_full_blast_radius);
        assert!(opts.include_rebuild_only);
        assert_eq!(opts.max_packages_listed, 7);
        assert_eq!(opts.max_top_blast_radius, 11);
    }

    #[test]
    fn options_default_matches_engine_constants() {
        let opts = ChangeSummaryOptions::default();
        assert!(opts.summary_enabled);
        assert!(opts.impact_enabled);
        assert!(!opts.compute_full_blast_radius);
        assert!(opts.include_rebuild_only);
        assert_eq!(opts.max_packages_listed, DEFAULT_MAX_PACKAGES_LISTED);
        assert_eq!(
            opts.max_top_blast_radius,
            impact::DEFAULT_MAX_TOP_BLAST_RADIUS
        );
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_summary_suppresses_rebuild_only_line_when_configured(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        // Same pname+version, different drv hash on each side → classified as RebuildOnly.
        let base_path = drv_path(H_BASE, "hello-2.13");
        let head_path = drv_path(H_HEAD, "hello-2.13");
        let base_eval = make_eval("hello", &base_path, "hello-2.13");
        let base_db = make_db_drv(&base_path, Some("hello"), Some("2.13"), None, None);
        let head_eval = make_eval("hello", &head_path, "hello-2.13");
        let head_db = make_db_drv(&head_path, Some("hello"), Some("2.13"), None, None);
        make_jobset(&pool, "base-sha", "ci", &[(base_eval, base_db)]).await;
        make_jobset(&pool, "head-sha", "ci", &[(head_eval, head_db)]).await;

        let graph = spawn_graph(&pool).await;

        // Default opts (include_rebuild_only=true): line is present.
        let summary_default = build_change_summary(
            &pool,
            &graph,
            "head-sha",
            "base-sha",
            "ci",
            &ChangeSummaryOptions::default(),
            None,
        )
        .await?
        .expect("expected Some summary");
        assert!(matches!(
            summary_default.package_changes[0],
            PackageChange::RebuildOnly { .. }
        ));
        assert!(
            summary_default
                .markdown
                .contains("rebuild without source changes")
        );

        // include_rebuild_only=false: line suppressed, footer not lying about a drop.
        let opts = ChangeSummaryOptions {
            include_rebuild_only: false,
            ..ChangeSummaryOptions::default()
        };
        let summary_off =
            build_change_summary(&pool, &graph, "head-sha", "base-sha", "ci", &opts, None)
                .await?
                .expect("expected Some summary");
        assert!(
            !summary_off
                .markdown
                .contains("rebuild without source changes")
        );
        assert!(!summary_off.truncated);
        Ok(())
    }

    // Tests below mutate EKACI_WORKSPACE_ROOT (process-global); serialize them
    // against each other to keep parallel test threads from clobbering reads.
    static WORKSPACE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set EKACI_WORKSPACE_ROOT to `path` and restore the previous value on drop.
    struct EnvGuard {
        prev: Option<String>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn set(path: &std::path::Path) -> Self {
            // Poisoned mutex still guards env access — recover and proceed.
            let lock = WORKSPACE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let prev = std::env::var("EKACI_WORKSPACE_ROOT").ok();
            unsafe {
                std::env::set_var("EKACI_WORKSPACE_ROOT", path);
            }
            Self { prev, _lock: lock }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.prev.take() {
                    Some(v) => std::env::set_var("EKACI_WORKSPACE_ROOT", v),
                    None => std::env::remove_var("EKACI_WORKSPACE_ROOT"),
                }
            }
        }
    }

    /// Write `<root>/github.com/<owner>/<repo>/worktrees/<sha>/.ekaci/config.json`.
    fn write_repo_config(
        root: &std::path::Path,
        owner: &str,
        repo: &str,
        sha: &str,
        body: &str,
    ) -> std::path::PathBuf {
        let mut p = root.to_path_buf();
        p.push("github.com");
        p.push(owner);
        p.push(repo);
        p.push("worktrees");
        p.push(sha);
        p.push(".ekaci");
        std::fs::create_dir_all(&p).unwrap();
        p.push("config.json");
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn load_repo_ci_config_returns_some_when_file_present() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = EnvGuard::set(tmp.path());
        write_repo_config(
            tmp.path(),
            "alice",
            "demo",
            "abc123",
            r#"{"jobs":{},"package_change_summary":{"enabled":false,"max_packages_listed":7}}"#,
        );
        let cfg = crate::ci::load_repo_ci_config("github.com", "alice", "demo", "abc123")
            .expect("expected Some(CIConfig)");
        let pcs = cfg.package_change_summary.expect("pcs block");
        assert!(!pcs.enabled);
        assert_eq!(pcs.max_packages_listed, 7);
    }

    #[test]
    fn load_repo_ci_config_returns_none_when_path_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let _g = EnvGuard::set(tmp.path());
        // No worktree written → loader returns None.
        let cfg = crate::ci::load_repo_ci_config("github.com", "alice", "demo", "abc123");
        assert!(cfg.is_none());
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn resolve_options_returns_defaults_when_jobset_missing(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let opts = resolve_options_for_jobset(&pool, "missing-sha", "ci").await;
        // Defaults match — no DB row, no env touched.
        assert!(opts.summary_enabled);
        assert!(opts.impact_enabled);
        assert!(opts.include_rebuild_only);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn resolve_options_returns_defaults_when_worktree_absent(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let head_path = drv_path(H_HEAD, "hello-2.13");
        let head_eval = make_eval("hello", &head_path, "hello-2.13");
        let head_db = make_db_drv(&head_path, Some("hello"), Some("2.13"), None, None);
        make_jobset(&pool, "head-sha", "ci", &[(head_eval, head_db)]).await;

        let tmp = tempfile::tempdir().unwrap();
        let _g = EnvGuard::set(tmp.path());
        let opts = resolve_options_for_jobset(&pool, "head-sha", "ci").await;
        assert!(opts.summary_enabled);
        assert_eq!(opts.max_packages_listed, DEFAULT_MAX_PACKAGES_LISTED);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn resolve_options_reads_config_from_worktree(pool: SqlitePool) -> anyhow::Result<()> {
        let head_path = drv_path(H_HEAD, "hello-2.13");
        let head_eval = make_eval("hello", &head_path, "hello-2.13");
        let head_db = make_db_drv(&head_path, Some("hello"), Some("2.13"), None, None);
        // make_jobset uses owner="owner", repo="repo" — match those in the worktree path.
        make_jobset(&pool, "head-sha", "ci", &[(head_eval, head_db)]).await;

        let tmp = tempfile::tempdir().unwrap();
        let _g = EnvGuard::set(tmp.path());
        write_repo_config(
            tmp.path(),
            "owner",
            "repo",
            "head-sha",
            r#"{
                "jobs": {},
                "package_change_summary": {
                    "enabled": true,
                    "max_packages_listed": 42,
                    "include_rebuild_only": false
                },
                "rebuild_impact": {
                    "enabled": false,
                    "max_top_blast_radius": 9,
                    "compute_full_blast_radius": true
                }
            }"#,
        );

        let opts = resolve_options_for_jobset(&pool, "head-sha", "ci").await;
        assert!(opts.summary_enabled);
        assert!(!opts.impact_enabled);
        assert!(opts.compute_full_blast_radius);
        assert!(!opts.include_rebuild_only);
        assert_eq!(opts.max_packages_listed, 42);
        assert_eq!(opts.max_top_blast_radius, 9);
        Ok(())
    }
}
