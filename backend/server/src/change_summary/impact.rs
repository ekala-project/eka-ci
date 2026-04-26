//! Rebuild impact analysis: per-system rebuild counts and per-package
//! blast radius (transitive-dependent count) for a `(head_sha, base_sha,
//! job)` triple. Counts come from `Job` SQL; blast radius rides the
//! in-memory `BuildGraph`. Wire types live in [`super::types`].

use std::collections::HashMap;

use anyhow::Context;
use sqlx::{Pool, Sqlite};

use super::cache;
use super::types::{PerSystemImpact, RebuildImpactResponse, TopBlastRadiusEntry};
use crate::db::model::DrvId;
use crate::graph::GraphServiceHandle;
use crate::metrics::ChangeSummaryMetrics;

/// Default cap on `top_blast_radius` rows per system.
pub const DEFAULT_MAX_TOP_BLAST_RADIUS: usize = 5;

/// Cap on per-system seeds eligible for full BFS in seeds-only mode.
pub const DEFAULT_MAX_BFS_SEEDS: usize = 50;

/// Seed row for the impact pass: drv_path identifies the seed, pname is a label, system partitions.
#[derive(Debug, Clone)]
struct ImpactRow {
    drv_path: DrvId,
    pname: Option<String>,
    system: String,
}

/// Build the rebuild-impact response for a `(head_sha, base_sha, job)` triple.
/// Per-system counts come from `Job WHERE difference IN (0,1)`; per-system top-N
/// blast radius comes from BFS seeded on the same set; `total_unique_drvs` is the
/// reverse-reachable closure across all seeds. `Ok(None)` when the head jobset
/// is missing. `base_sha` is informational (caching key) — diff already lives in `Job.difference`.
pub async fn build_rebuild_impact_response(
    pool: &Pool<Sqlite>,
    graph: &GraphServiceHandle,
    head_sha: &str,
    base_sha: &str,
    job: &str,
    max_top_blast_radius: usize,
    full_blast_radius: bool,
    metrics: Option<&ChangeSummaryMetrics>,
) -> anyhow::Result<Option<RebuildImpactResponse>> {
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

    // Seeds for the impact pass: jobs with difference IN (0,1). Removed (=2) doesn't rebuild here.
    let rows = load_changed_impact_rows(pool, head_jobset_id).await?;

    if let Some(m) = metrics {
        m.rebuild_impact_seeds.observe(rows.len() as f64);
    }

    // Group seeds by system for per-system top-K reporting.
    let mut seeds_by_system: HashMap<String, Vec<ImpactRow>> = HashMap::new();
    for row in &rows {
        seeds_by_system
            .entry(row.system.clone())
            .or_default()
            .push(row.clone());
    }

    // Union BFS across all seeds → `total_unique_drvs`.
    let all_seeds: Vec<DrvId> = rows.iter().map(|r| r.drv_path.clone()).collect();
    let union_reachable = graph
        .reverse_reachable_from_set(all_seeds.clone())
        .await
        .context("Failed to compute union reverse-reachable set")?;
    let total_unique_drvs = union_reachable.len();

    // Per-system rebuild counts + top-K blast radius (sorted for deterministic output).
    let mut per_system: Vec<PerSystemImpact> = Vec::with_capacity(seeds_by_system.len());
    let mut system_keys: Vec<String> = seeds_by_system.keys().cloned().collect();
    system_keys.sort();

    for system in system_keys {
        let mut rows_for_system = seeds_by_system.remove(&system).unwrap_or_default();
        let rebuild_count = rows_for_system.len();

        // Seeds-only mode caps the BFS-eligible seed set; full mode evaluates every seed.
        if !full_blast_radius && rows_for_system.len() > DEFAULT_MAX_BFS_SEEDS {
            rows_for_system.sort_by(|a, b| (*a.drv_path).cmp(&*b.drv_path));
            rows_for_system.truncate(DEFAULT_MAX_BFS_SEEDS);
        }

        let traversal_start = std::time::Instant::now();

        // Per-seed blast radius for ranking; cost is dominated by the BFS.
        let seed_ids: Vec<DrvId> = rows_for_system.iter().map(|r| r.drv_path.clone()).collect();
        let radii = graph
            .blast_radius_per_seed(seed_ids)
            .await
            .with_context(|| format!("Failed to compute blast radii for system {system}"))?;

        if let Some(m) = metrics {
            m.rebuild_impact_traversal_duration_seconds
                .with_label_values(&[&system])
                .observe(traversal_start.elapsed().as_secs_f64());
        }

        let mut entries: Vec<TopBlastRadiusEntry> = rows_for_system
            .iter()
            .map(|row| TopBlastRadiusEntry {
                pname: row.pname.clone(),
                drv_path: row.drv_path.clone(),
                blast_radius: radii.get(&row.drv_path).copied().unwrap_or(0),
            })
            .collect();

        // Largest radius first; tie-break by drv_path for stable ordering.
        entries.sort_by(|a, b| {
            b.blast_radius
                .cmp(&a.blast_radius)
                .then_with(|| (*a.drv_path).cmp(&*b.drv_path))
        });
        entries.truncate(max_top_blast_radius.max(1));

        per_system.push(PerSystemImpact {
            system,
            rebuild_count,
            top_blast_radius: entries,
        });
    }

    let computed_at = chrono::Utc::now().to_rfc3339();

    Ok(Some(RebuildImpactResponse {
        head_sha: head_sha.to_string(),
        base_sha: base_sha.to_string(),
        job: job.to_string(),
        computed_at,
        per_system,
        total_unique_drvs,
    }))
}

/// Read-through cached wrapper around [`build_rebuild_impact_response`].
/// `Ok(None)` (head jobset missing) is not cached; cache write failures log at WARN.
pub async fn build_rebuild_impact_response_cached(
    pool: &Pool<Sqlite>,
    graph: &GraphServiceHandle,
    head_sha: &str,
    base_sha: &str,
    job: &str,
    max_top_blast_radius: usize,
    full_blast_radius: bool,
    metrics: Option<&ChangeSummaryMetrics>,
) -> anyhow::Result<Option<RebuildImpactResponse>> {
    if let Some(hit) = cache::lookup(pool, head_sha, base_sha, job, full_blast_radius).await? {
        if let Some(m) = metrics {
            m.cache_hits_total.inc();
        }
        return Ok(Some(hit));
    }

    if let Some(m) = metrics {
        m.cache_misses_total.inc();
    }

    let computed = build_rebuild_impact_response(
        pool,
        graph,
        head_sha,
        base_sha,
        job,
        max_top_blast_radius,
        full_blast_radius,
        metrics,
    )
    .await?;

    let Some(response) = computed else {
        return Ok(None);
    };

    if let Err(err) = cache::upsert(pool, &response, full_blast_radius).await {
        tracing::warn!(
            error = %err,
            head_sha,
            base_sha,
            job,
            "Failed to write RebuildImpactCache entry; serving uncached response"
        );
    }

    Ok(Some(response))
}

/// Load rows for impact analysis: every (drv_path, pname, system) for the
/// jobset where `Job.difference IN (0 New, 1 Changed)`.
///
/// Returns rows in insertion order (no explicit ORDER BY); callers must
/// not rely on order beyond what `compute_package_changes` callers expect.
async fn load_changed_impact_rows(
    pool: &Pool<Sqlite>,
    jobset_id: i64,
) -> anyhow::Result<Vec<ImpactRow>> {
    let raw: Vec<(String, Option<String>, String)> = sqlx::query_as(
        r#"
        SELECT d.drv_path, d.pname, d.system
        FROM Job j
        INNER JOIN Drv d ON j.drv_id = d.ROWID
        WHERE j.jobset = ? AND j.difference IN (0, 1)
        "#,
    )
    .bind(jobset_id)
    .fetch_all(pool)
    .await
    .context("Failed to load impact rows for jobset")?;

    raw.into_iter()
        .map(|(drv_path_str, pname, system)| {
            let drv_path = DrvId::try_from(drv_path_str.as_str())
                .with_context(|| format!("Invalid drv_path in DB: {drv_path_str}"))?;
            Ok(ImpactRow {
                drv_path,
                pname,
                system,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::SqlitePool;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::db::DbService;
    use crate::db::github::{create_jobs_for_jobset, create_jobset, job_difference};
    use crate::db::model::build_event::DrvBuildState;
    use crate::db::model::drv::{Drv, insert_drv};
    use crate::db::model::drv_id::DrvId;
    use crate::graph::{GraphCommand, GraphService};
    use crate::nix::nix_eval_jobs::NixEvalDrv;

    /// Spin up a real `GraphService` against a test pool and return a handle
    /// + a guard that aborts the service task on drop. Mirrors the pattern
    /// used by `tests/service_integration.rs` but inlined to avoid pulling
    /// the `tests/common` module into a unit-test path.
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

    /// Build a minimal `Drv` row sufficient for impact analysis.
    fn make_db_drv(drv_path: &str, pname: Option<&str>, system: &str) -> Drv {
        Drv {
            drv_path: DrvId::from_str(drv_path).unwrap(),
            system: system.to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: pname.map(str::to_string),
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        }
    }

    /// Sanitize a tag to nix base32 (alphabet excludes `e`, `o`, `t`, `u`)
    /// and zero-pad to 32 chars. Mirrors the helper in
    /// `graph::graph::tests::make_test_drv` so we can use readable tags.
    fn pad_hash(prefix: &str) -> String {
        let sanitized: String = prefix
            .chars()
            .map(|c| match c {
                'e' => 'f',
                'o' => 'p',
                't' => 's',
                'u' => 'v',
                other => other,
            })
            .collect();
        format!("{:0>32}", sanitized)
    }

    fn drv_path(prefix: &str, name: &str) -> String {
        let hash = pad_hash(prefix);
        format!("/nix/store/{hash}-{name}.drv")
    }

    /// Construct a (sha, job) jobset and return its id. Inserts both `Drv`
    /// rows and `Job` rows.
    async fn make_jobset(
        pool: &SqlitePool,
        sha: &str,
        job: &str,
        rows: &[(NixEvalDrv, Drv)],
    ) -> i64 {
        let jobset_id = create_jobset(sha, job, "owner", "repo", None, pool)
            .await
            .expect("create_jobset failed");
        for (_e, drv) in rows {
            insert_drv(pool, drv).await.expect("insert_drv failed");
        }
        let evals: Vec<NixEvalDrv> = rows.iter().map(|(e, _)| e.clone()).collect();
        create_jobs_for_jobset(jobset_id, &evals, pool)
            .await
            .expect("create_jobs_for_jobset failed");
        jobset_id
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_impact_returns_none_when_head_jobset_missing(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        let graph = spawn_graph(&pool).await;
        let resp = build_rebuild_impact_response(
            &pool,
            &graph,
            "missing-sha",
            "base-sha",
            "ci",
            5,
            false,
            None,
        )
        .await?;
        assert!(resp.is_none());
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_impact_counts_changed_drvs_per_system(pool: SqlitePool) -> anyhow::Result<()> {
        // Base jobset: hello-2.12.
        let base_path = drv_path("base", "hello-2.12");
        let base_eval = make_eval("hello", &base_path, "hello-2.12");
        let base_db = make_db_drv(&base_path, Some("hello"), "x86_64-linux");
        make_jobset(&pool, "base-sha", "ci", &[(base_eval, base_db)]).await;

        // Head jobset: hello-2.13 (Changed) + new-pkg (New).
        let h1 = drv_path("head1", "hello-2.13");
        let h2 = drv_path("head2", "new-pkg-1.0");
        let h1_eval = make_eval("hello", &h1, "hello-2.13");
        let h2_eval = make_eval("new-pkg", &h2, "new-pkg-1.0");
        let h1_db = make_db_drv(&h1, Some("hello"), "x86_64-linux");
        let h2_db = make_db_drv(&h2, Some("new-pkg"), "x86_64-linux");
        make_jobset(
            &pool,
            "head-sha",
            "ci",
            &[(h1_eval, h1_db), (h2_eval, h2_db)],
        )
        .await;

        // Run job_difference so the head jobset's `Job.difference` is set
        // (otherwise everything stays at default `New`, which still works
        // for this test but is closer to production behaviour).
        let _ = job_difference("head-sha", "base-sha", "ci", &pool).await?;

        let graph = spawn_graph(&pool).await;
        let resp = build_rebuild_impact_response(
            &pool, &graph, "head-sha", "base-sha", "ci", 5, false, None,
        )
        .await?
        .expect("expected Some response");

        assert_eq!(resp.head_sha, "head-sha");
        assert_eq!(resp.base_sha, "base-sha");
        assert_eq!(resp.job, "ci");
        assert_eq!(resp.per_system.len(), 1);
        let sys = &resp.per_system[0];
        assert_eq!(sys.system, "x86_64-linux");
        // Both head drvs are New/Changed → both contribute to rebuild_count.
        assert_eq!(sys.rebuild_count, 2);
        assert_eq!(sys.top_blast_radius.len(), 2);
        // Empty graph (no edges in DrvRefs) → all blast radii are 0.
        for entry in &sys.top_blast_radius {
            assert_eq!(entry.blast_radius, 0);
        }
        // total_unique_drvs equals the seed count when no dependents exist
        // (each seed contributes itself only).
        assert_eq!(resp.total_unique_drvs, 2);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_impact_partitions_by_system(pool: SqlitePool) -> anyhow::Result<()> {
        // Two systems on the head side; no base ⇒ everything is New.
        let p_x86 = drv_path("aaa1", "pkg-x86");
        let p_arm = drv_path("aaa2", "pkg-arm");
        let mut e_x86 = make_eval("pkg-x86", &p_x86, "pkg-x86");
        e_x86.system = "x86_64-linux".to_string();
        let mut e_arm = make_eval("pkg-arm", &p_arm, "pkg-arm");
        e_arm.system = "aarch64-linux".to_string();
        let d_x86 = make_db_drv(&p_x86, Some("pkg-x86"), "x86_64-linux");
        let d_arm = make_db_drv(&p_arm, Some("pkg-arm"), "aarch64-linux");
        make_jobset(&pool, "head-sha", "ci", &[(e_x86, d_x86), (e_arm, d_arm)]).await;

        let graph = spawn_graph(&pool).await;
        let resp = build_rebuild_impact_response(
            &pool,
            &graph,
            "head-sha",
            "missing-base",
            "ci",
            5,
            false,
            None,
        )
        .await?
        .expect("expected Some response");

        assert_eq!(resp.per_system.len(), 2);
        // Deterministic alphabetical ordering of systems.
        assert_eq!(resp.per_system[0].system, "aarch64-linux");
        assert_eq!(resp.per_system[1].system, "x86_64-linux");
        for sys in &resp.per_system {
            assert_eq!(sys.rebuild_count, 1);
            assert_eq!(sys.top_blast_radius.len(), 1);
        }
        // Two distinct seeds ⇒ total_unique_drvs == 2 (no edges).
        assert_eq!(resp.total_unique_drvs, 2);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn build_impact_truncates_top_blast_radius(pool: SqlitePool) -> anyhow::Result<()> {
        // 5 drvs, ask for top 2.
        let mut rows = Vec::new();
        for i in 0..5 {
            let path = drv_path(&format!("kk{i}"), &format!("pkg{i}-1.0"));
            let eval = make_eval(&format!("pkg{i}"), &path, &format!("pkg{i}-1.0"));
            let db = make_db_drv(&path, Some(&format!("pkg{i}")), "x86_64-linux");
            rows.push((eval, db));
        }
        make_jobset(&pool, "head-sha", "ci", &rows).await;

        let graph = spawn_graph(&pool).await;
        let resp = build_rebuild_impact_response(
            &pool,
            &graph,
            "head-sha",
            "missing-base",
            "ci",
            2,
            false,
            None,
        )
        .await?
        .expect("expected Some response");

        assert_eq!(resp.per_system.len(), 1);
        let sys = &resp.per_system[0];
        // rebuild_count is the un-truncated count — it counts all New
        // changes for the system, regardless of top_k.
        assert_eq!(sys.rebuild_count, 5);
        assert_eq!(sys.top_blast_radius.len(), 2);
        Ok(())
    }

    /// First call computes; the cache row should appear and a second
    /// call should be served from cache. We assert the second response
    /// is byte-equal (after JSON round-trip) and that exactly one cache
    /// row exists.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn cached_first_call_populates_cache(pool: SqlitePool) -> anyhow::Result<()> {
        let path = drv_path("aaa1", "hello-1.0");
        let eval = make_eval("hello", &path, "hello-1.0");
        let db = make_db_drv(&path, Some("hello"), "x86_64-linux");
        make_jobset(&pool, "head-sha", "ci", &[(eval, db)]).await;

        let graph = spawn_graph(&pool).await;

        let first = build_rebuild_impact_response_cached(
            &pool,
            &graph,
            "head-sha",
            "missing-base",
            "ci",
            5,
            false,
            None,
        )
        .await?
        .expect("expected Some on first call");

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM RebuildImpactCache")
            .fetch_one(&pool)
            .await?;
        assert_eq!(
            count, 1,
            "cache should have exactly one entry after compute"
        );

        let second = build_rebuild_impact_response_cached(
            &pool,
            &graph,
            "head-sha",
            "missing-base",
            "ci",
            5,
            false,
            None,
        )
        .await?
        .expect("expected Some on second call");

        // Wire-equivalent (modulo `computed_at` which is preserved
        // verbatim from the cache row).
        assert_eq!(first.head_sha, second.head_sha);
        assert_eq!(first.base_sha, second.base_sha);
        assert_eq!(first.job, second.job);
        assert_eq!(first.total_unique_drvs, second.total_unique_drvs);
        assert_eq!(first.computed_at, second.computed_at);
        assert_eq!(first.per_system.len(), second.per_system.len());
        assert_eq!(
            first.per_system[0].rebuild_count,
            second.per_system[0].rebuild_count
        );
        Ok(())
    }

    /// A `head-sha` with no jobset should propagate as `Ok(None)` and
    /// must NOT poison the cache with a row.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn cached_404_does_not_populate_cache(pool: SqlitePool) -> anyhow::Result<()> {
        let graph = spawn_graph(&pool).await;
        let result = build_rebuild_impact_response_cached(
            &pool,
            &graph,
            "missing-sha",
            "missing-base",
            "ci",
            5,
            false,
            None,
        )
        .await?;
        assert!(result.is_none());

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM RebuildImpactCache")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 0, "404 must not produce a cache row");
        Ok(())
    }

    /// A pre-existing cache row should be returned verbatim; the
    /// underlying computation should NOT run (we verify this indirectly
    /// by inserting a synthetic row whose `total_unique_drvs` differs
    /// from what the live computation would yield).
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn cached_lookup_short_circuits_computation(pool: SqlitePool) -> anyhow::Result<()> {
        let path = drv_path("bbb1", "hello-1.0");
        let eval = make_eval("hello", &path, "hello-1.0");
        let db = make_db_drv(&path, Some("hello"), "x86_64-linux");
        make_jobset(&pool, "head-sha", "ci", &[(eval, db)]).await;

        // Pre-populate the cache with a sentinel total_unique_drvs that
        // a fresh compute could never produce (live compute would be 1).
        let sentinel = RebuildImpactResponse {
            head_sha: "head-sha".to_string(),
            base_sha: "missing-base".to_string(),
            job: "ci".to_string(),
            computed_at: "2026-04-25T00:00:00Z".to_string(),
            per_system: vec![],
            total_unique_drvs: 9999,
        };
        cache::upsert(&pool, &sentinel, false).await?;

        let graph = spawn_graph(&pool).await;
        let got = build_rebuild_impact_response_cached(
            &pool,
            &graph,
            "head-sha",
            "missing-base",
            "ci",
            5,
            false,
            None,
        )
        .await?
        .expect("expected Some response");

        assert_eq!(got.total_unique_drvs, 9999);
        assert!(got.per_system.is_empty());
        Ok(())
    }
}
