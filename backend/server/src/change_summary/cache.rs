//! Read-through cache for [`RebuildImpactResponse`] computations.
//!
//! Implements the `RebuildImpactCache` table from
//! `docs/design-package-change-rebuild-impact.md` §5.2 + §10.
//!
//! Cache key: `(head_sha, base_sha, jobset)`. The cached value is the
//! full serialised response in `summary_json`, plus a denormalised
//! "headline" (rebuild_count, critical_path_drv, blast_radius) for cheap
//! aggregate lookups.
//!
//! The cache is **read-through**: callers should:
//!   1. [`lookup`] the (head, base, job) triple.
//!   2. On hit, return the deserialised response.
//!   3. On miss, compute fresh and call [`upsert`] before returning.
//!
//! Invalidation is implicit: a new `head_sha` produces a new PK row.
//! Stale entries are pruned by [`cleanup_old_entries`] which the server
//! invokes once at startup (and could be re-invoked nightly).
//!
//! ## Headline derivation
//!
//! `rebuild_count` is the sum of `rebuild_count` across all `per_system`
//! entries — the total number of rebuild "slots" if the PR landed.
//! `critical_path_drv` and `blast_radius` track the single most-impactful
//! drv across all systems; ties broken by alphabetic `drv_path` order
//! (matches the per-system `top_blast_radius` ordering).

use anyhow::Context;
use serde::Deserialize;
use sqlx::{Pool, Sqlite};

use super::types::RebuildImpactResponse;

/// Default retention window for cache entries.
pub const DEFAULT_CACHE_TTL_DAYS: i64 = 7;

/// Look up a cached rebuild-impact response by `(head_sha, base_sha,
/// jobset)` triple. Returns `Ok(None)` on miss. A row whose
/// `summary_json` fails to deserialise is treated as a miss (and logged)
/// so a schema bump never poisons the cache.
pub async fn lookup(
    pool: &Pool<Sqlite>,
    head_sha: &str,
    base_sha: &str,
    jobset: &str,
) -> anyhow::Result<Option<RebuildImpactResponse>> {
    let row: Option<(String,)> = sqlx::query_as(
        r#"
        SELECT summary_json
        FROM RebuildImpactCache
        WHERE head_sha = ? AND base_sha = ? AND jobset = ?
        "#,
    )
    .bind(head_sha)
    .bind(base_sha)
    .bind(jobset)
    .fetch_optional(pool)
    .await
    .context("Failed to query RebuildImpactCache")?;

    let Some((summary_json,)) = row else {
        return Ok(None);
    };

    // `RebuildImpactResponse` itself only derives `Serialize`. We define
    // an internal mirror struct deriving `Deserialize` here so we can
    // round-trip without changing the wire-type derive set.
    match serde_json::from_str::<CachedResponse>(&summary_json) {
        Ok(cached) => Ok(Some(cached.into_response())),
        Err(err) => {
            tracing::warn!(
                error = %err,
                head_sha,
                base_sha,
                jobset,
                "Failed to deserialise cached RebuildImpactResponse; treating as miss"
            );
            Ok(None)
        },
    }
}

/// Insert-or-replace a cache row for the given response.
///
/// The denormalised headline columns (`rebuild_count`,
/// `critical_path_drv`, `blast_radius`) are derived from the response
/// itself: rebuild_count is the sum across all systems, and the
/// critical-path drv is the highest-blast-radius entry across all
/// systems (ties broken lexicographically by `drv_path`).
pub async fn upsert(pool: &Pool<Sqlite>, response: &RebuildImpactResponse) -> anyhow::Result<()> {
    let summary_json =
        serde_json::to_string(response).context("Failed to serialise RebuildImpactResponse")?;

    let rebuild_count: i64 = response
        .per_system
        .iter()
        .map(|s| s.rebuild_count as i64)
        .sum();

    // Pick the single highest-blast-radius entry across all systems.
    let mut top: Option<(&str, usize)> = None;
    for sys in &response.per_system {
        for entry in &sys.top_blast_radius {
            let candidate = (entry.drv_path.as_ref(), entry.blast_radius);
            top = match top {
                None => Some(candidate),
                Some((cur_drv, cur_radius)) => {
                    if candidate.1 > cur_radius
                        || (candidate.1 == cur_radius && candidate.0 < cur_drv)
                    {
                        Some(candidate)
                    } else {
                        Some((cur_drv, cur_radius))
                    }
                },
            };
        }
    }

    let (critical_path_drv, blast_radius): (Option<String>, Option<i64>) = match top {
        Some((drv, radius)) => (Some(drv.to_string()), Some(radius as i64)),
        None => (None, None),
    };

    sqlx::query(
        r#"
        INSERT INTO RebuildImpactCache
            (head_sha, base_sha, jobset, rebuild_count,
             critical_path_drv, blast_radius, summary_json, computed_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
        ON CONFLICT(head_sha, base_sha, jobset) DO UPDATE SET
            rebuild_count = excluded.rebuild_count,
            critical_path_drv = excluded.critical_path_drv,
            blast_radius = excluded.blast_radius,
            summary_json = excluded.summary_json,
            computed_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(&response.head_sha)
    .bind(&response.base_sha)
    .bind(&response.job)
    .bind(rebuild_count)
    .bind(&critical_path_drv)
    .bind(blast_radius)
    .bind(&summary_json)
    .execute(pool)
    .await
    .context("Failed to upsert RebuildImpactCache row")?;

    Ok(())
}

/// Delete cache rows older than `threshold_days`. Returns the number of
/// rows deleted.
///
/// Called from server startup to prevent unbounded growth. Per design
/// §5.2, a 7-day window is the default; operators with bursty workloads
/// can tune this.
pub async fn cleanup_old_entries(pool: &Pool<Sqlite>, threshold_days: i64) -> anyhow::Result<u64> {
    // SQLite's datetime modifier wants a string like '-7 days'.
    let modifier = format!("-{threshold_days} days");
    let result = sqlx::query(
        r#"
        DELETE FROM RebuildImpactCache
        WHERE computed_at < datetime('now', ?)
        "#,
    )
    .bind(&modifier)
    .execute(pool)
    .await
    .context("Failed to clean up stale RebuildImpactCache rows")?;
    Ok(result.rows_affected())
}

// --- internal deserialisation mirrors -------------------------------------
//
// The wire types in `super::types` are `Serialize`-only by design (they
// are response-shaped). To round-trip from the cache we keep parallel
// `Deserialize` mirror structs here, scoped private to this module.

#[derive(Debug, Deserialize)]
struct CachedResponse {
    head_sha: String,
    base_sha: String,
    job: String,
    computed_at: String,
    per_system: Vec<CachedPerSystem>,
    total_unique_drvs: usize,
}

#[derive(Debug, Deserialize)]
struct CachedPerSystem {
    system: String,
    rebuild_count: usize,
    top_blast_radius: Vec<CachedTopEntry>,
}

#[derive(Debug, Deserialize)]
struct CachedTopEntry {
    pname: Option<String>,
    drv_path: String,
    blast_radius: usize,
}

impl CachedResponse {
    fn into_response(self) -> RebuildImpactResponse {
        use std::str::FromStr;

        use super::types::{PerSystemImpact, TopBlastRadiusEntry};
        use crate::db::model::DrvId;

        let per_system = self
            .per_system
            .into_iter()
            .map(|s| PerSystemImpact {
                system: s.system,
                rebuild_count: s.rebuild_count,
                top_blast_radius: s
                    .top_blast_radius
                    .into_iter()
                    .filter_map(|e| {
                        // A cached `drv_path` that fails validation is dropped
                        // rather than panicking. This can only happen if the
                        // schema for DrvId tightens after the row was written.
                        let drv_path = DrvId::from_str(&e.drv_path).ok()?;
                        Some(TopBlastRadiusEntry {
                            pname: e.pname,
                            drv_path,
                            blast_radius: e.blast_radius,
                        })
                    })
                    .collect(),
            })
            .collect();

        RebuildImpactResponse {
            head_sha: self.head_sha,
            base_sha: self.base_sha,
            job: self.job,
            computed_at: self.computed_at,
            per_system,
            total_unique_drvs: self.total_unique_drvs,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use sqlx::SqlitePool;

    use super::*;
    use crate::change_summary::types::{PerSystemImpact, TopBlastRadiusEntry};
    use crate::db::model::DrvId;

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

    fn dummy_drv(prefix: &str, name: &str) -> DrvId {
        let hash = pad_hash(prefix);
        DrvId::from_str(&format!("/nix/store/{hash}-{name}.drv")).unwrap()
    }

    fn make_response(head: &str, base: &str, job: &str) -> RebuildImpactResponse {
        RebuildImpactResponse {
            head_sha: head.to_string(),
            base_sha: base.to_string(),
            job: job.to_string(),
            computed_at: "2026-04-25T00:00:00Z".to_string(),
            per_system: vec![PerSystemImpact {
                system: "x86_64-linux".to_string(),
                rebuild_count: 3,
                top_blast_radius: vec![
                    TopBlastRadiusEntry {
                        pname: Some("alpha".to_string()),
                        drv_path: dummy_drv("aaa1", "alpha-1.0"),
                        blast_radius: 7,
                    },
                    TopBlastRadiusEntry {
                        pname: Some("beta".to_string()),
                        drv_path: dummy_drv("bbb1", "beta-1.0"),
                        blast_radius: 2,
                    },
                ],
            }],
            total_unique_drvs: 9,
        }
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn lookup_returns_none_on_miss(pool: SqlitePool) -> anyhow::Result<()> {
        let result = lookup(&pool, "nope", "nope2", "ci").await?;
        assert!(result.is_none());
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn upsert_then_lookup_round_trips(pool: SqlitePool) -> anyhow::Result<()> {
        let resp = make_response("h1", "b1", "ci");
        upsert(&pool, &resp).await?;

        let got = lookup(&pool, "h1", "b1", "ci")
            .await?
            .expect("cache hit expected");

        assert_eq!(got.head_sha, "h1");
        assert_eq!(got.base_sha, "b1");
        assert_eq!(got.job, "ci");
        assert_eq!(got.total_unique_drvs, 9);
        assert_eq!(got.per_system.len(), 1);
        assert_eq!(got.per_system[0].rebuild_count, 3);
        assert_eq!(got.per_system[0].top_blast_radius.len(), 2);
        assert_eq!(got.per_system[0].top_blast_radius[0].blast_radius, 7);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn upsert_replaces_existing_row(pool: SqlitePool) -> anyhow::Result<()> {
        let mut resp = make_response("h1", "b1", "ci");
        upsert(&pool, &resp).await?;

        // Mutate and re-insert.
        resp.total_unique_drvs = 42;
        resp.per_system[0].rebuild_count = 99;
        upsert(&pool, &resp).await?;

        let got = lookup(&pool, "h1", "b1", "ci")
            .await?
            .expect("cache hit expected");
        assert_eq!(got.total_unique_drvs, 42);
        assert_eq!(got.per_system[0].rebuild_count, 99);

        // Still exactly one row.
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM RebuildImpactCache")
            .fetch_one(&pool)
            .await?;
        assert_eq!(count, 1);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn upsert_records_headline_columns(pool: SqlitePool) -> anyhow::Result<()> {
        let resp = make_response("h1", "b1", "ci");
        upsert(&pool, &resp).await?;

        let row: (i64, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT rebuild_count, critical_path_drv, blast_radius FROM RebuildImpactCache",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(row.0, 3);
        assert_eq!(row.2, Some(7));
        // The "alpha" entry is the highest-blast-radius drv.
        assert!(
            row.1
                .as_deref()
                .map(|s| s.contains("alpha"))
                .unwrap_or(false),
            "critical_path_drv = {:?}",
            row.1
        );
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn cleanup_removes_old_rows(pool: SqlitePool) -> anyhow::Result<()> {
        // Insert a row, then back-date it past the threshold.
        let resp = make_response("old-head", "old-base", "ci");
        upsert(&pool, &resp).await?;
        sqlx::query("UPDATE RebuildImpactCache SET computed_at = datetime('now', '-30 days')")
            .execute(&pool)
            .await?;

        // Insert a fresh row that should survive cleanup.
        let resp2 = make_response("new-head", "new-base", "ci");
        upsert(&pool, &resp2).await?;

        let deleted = cleanup_old_entries(&pool, DEFAULT_CACHE_TTL_DAYS).await?;
        assert_eq!(deleted, 1);

        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM RebuildImpactCache")
            .fetch_one(&pool)
            .await?;
        assert_eq!(remaining, 1);

        // Confirm it's the new row that survived.
        let head: String = sqlx::query_scalar("SELECT head_sha FROM RebuildImpactCache")
            .fetch_one(&pool)
            .await?;
        assert_eq!(head, "new-head");
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn lookup_treats_corrupt_json_as_miss(pool: SqlitePool) -> anyhow::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO RebuildImpactCache
                (head_sha, base_sha, jobset, rebuild_count, summary_json)
            VALUES ('h', 'b', 'ci', 0, 'not json')
            "#,
        )
        .execute(&pool)
        .await?;

        let got = lookup(&pool, "h", "b", "ci").await?;
        assert!(got.is_none());
        Ok(())
    }
}
