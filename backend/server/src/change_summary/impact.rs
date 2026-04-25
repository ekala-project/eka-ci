//! Rebuild impact analysis (A2).
//!
//! Computes per-system rebuild counts and per-package "blast radius"
//! (transitive-dependent count) for a `(head_sha, base_sha, job)` triple.
//!
//! Boundaries:
//! - Per-system **counts** (`rebuild_count`) come from the `Job` table — `difference IN (0 New, 1
//!   Changed)` — and are computed entirely in SQL. See design §8.1.
//! - **Blast radius** is computed off the in-memory `BuildGraph` via
//!   [`crate::graph::GraphServiceHandle::blast_radius_per_seed`] and
//!   [`crate::graph::GraphServiceHandle::reverse_reachable_from_set`]. See design §8.2.
//!
//! The wire types (P2 surface) live alongside [`super::types`] so the P4
//! markdown renderer and an upstream aggregated `change-summary` endpoint
//! can pull them in without depending on this implementation module.

use std::collections::HashMap;

use anyhow::Context;
use sqlx::{Pool, Sqlite};

use super::types::{PerSystemImpact, RebuildImpactResponse, TopBlastRadiusEntry};
use crate::db::model::DrvId;
use crate::graph::GraphServiceHandle;

/// Default top-N cap for `top_blast_radius` per system, per design §11.1
/// (`max_top_blast_radius`).
pub const DEFAULT_MAX_TOP_BLAST_RADIUS: usize = 5;

/// One row from the impact-side `Job ⋈ Drv` join: just the columns this
/// module needs to seed BFS and report human labels.
///
/// Distinct from `change_summary::classify::JobDrvRow` because impact does
/// not care about license/maintainer JSON or the Job.name<->attr_path tie:
/// the seed identity is the **drv_path**, and pname is purely a label.
#[derive(Debug, Clone)]
struct ImpactRow {
    drv_path: DrvId,
    pname: Option<String>,
    system: String,
}

/// Public entry point: build the rebuild-impact response for a triple.
///
/// Behaviour:
/// 1. Resolve `(head_sha, job)` → `head_jobset_id`. Returns `Ok(None)` when missing (caller maps to
///    404).
/// 2. Per-system rebuild counts come from `COUNT(*) FROM Job WHERE jobset = ? AND difference IN
///    (0,1)` partitioned by `Drv.system`. Removed rows (difference = 2) are excluded — they don't
///    trigger a rebuild on the head side.
/// 3. Per-system seeds = drvs with `difference IN (0,1)` (i.e., New or Changed). For each system,
///    we ask the graph for `blast_radius_per_seed`, sort descending, take top-N.
/// 4. `total_unique_drvs` = `|reverse_reachable_from_set(all_seeds)|`. This is the number of drvs
///    that would have to rebuild somewhere across all systems if the changed seeds rebuild.
///
/// `base_sha` is currently informational only; the `Job.difference` column
/// is already populated against the head jobset's evaluation so we don't
/// need to re-diff. We accept it in the signature to match the
/// package-changes endpoint shape and to make a future caching layer
/// (`RebuildImpactCache`, P3) trivial to slot in.
pub async fn build_rebuild_impact_response(
    pool: &Pool<Sqlite>,
    graph: &GraphServiceHandle,
    head_sha: &str,
    base_sha: &str,
    job: &str,
    max_top_blast_radius: usize,
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

    // Load all (drv_path, pname, system) for this jobset where the job
    // contributes to a rebuild. We deliberately **exclude** difference=2
    // (Removed) because those don't rebuild on the head side.
    let rows = load_changed_impact_rows(pool, head_jobset_id).await?;

    // Group seeds by system for per-system top-K reporting.
    let mut seeds_by_system: HashMap<String, Vec<ImpactRow>> = HashMap::new();
    for row in &rows {
        seeds_by_system
            .entry(row.system.clone())
            .or_default()
            .push(row.clone());
    }

    // Compute the union BFS once across all seeds. This is the
    // total_unique_drvs (= every drv that would rebuild somewhere).
    let all_seeds: Vec<DrvId> = rows.iter().map(|r| r.drv_path.clone()).collect();
    let union_reachable = graph
        .reverse_reachable_from_set(all_seeds.clone())
        .await
        .context("Failed to compute union reverse-reachable set")?;
    let total_unique_drvs = union_reachable.len();

    // Per-system rebuild counts + top-K blast radius.
    let mut per_system: Vec<PerSystemImpact> = Vec::with_capacity(seeds_by_system.len());
    // Sort systems for deterministic output.
    let mut system_keys: Vec<String> = seeds_by_system.keys().cloned().collect();
    system_keys.sort();

    for system in system_keys {
        let rows_for_system = seeds_by_system.remove(&system).unwrap_or_default();
        let rebuild_count = rows_for_system.len();

        // Per-seed blast radius for ranking. Cheap relative to BFS — cost
        // dominated by the BFS itself.
        let seed_ids: Vec<DrvId> = rows_for_system.iter().map(|r| r.drv_path.clone()).collect();
        let radii = graph
            .blast_radius_per_seed(seed_ids)
            .await
            .with_context(|| format!("Failed to compute blast radii for system {system}"))?;

        // Pair rows with their radius so we can sort and label.
        let mut entries: Vec<TopBlastRadiusEntry> = rows_for_system
            .iter()
            .map(|row| TopBlastRadiusEntry {
                pname: row.pname.clone(),
                drv_path: row.drv_path.clone(),
                blast_radius: radii.get(&row.drv_path).copied().unwrap_or(0),
            })
            .collect();

        // Largest radius first; tie-break by drv_path for determinism.
        // `DrvId` derefs to `str`, giving us a stable lexicographic order.
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
        let resp = build_rebuild_impact_response(&pool, &graph, "missing-sha", "base-sha", "ci", 5)
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
        let resp = build_rebuild_impact_response(&pool, &graph, "head-sha", "base-sha", "ci", 5)
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
        let resp =
            build_rebuild_impact_response(&pool, &graph, "head-sha", "missing-base", "ci", 5)
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
        let resp =
            build_rebuild_impact_response(&pool, &graph, "head-sha", "missing-base", "ci", 2)
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
}
