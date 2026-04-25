use sqlx::{Pool, Sqlite, SqlitePool};
use tracing::{debug, info};

use super::Drv;
use crate::db::model::build_event::DrvBuildState;
use crate::db::model::{DrvId, Reference, Referrer, drv_id};

pub async fn get_drv(derivation: &DrvId, pool: &Pool<Sqlite>) -> anyhow::Result<Option<Drv>> {
    let event = sqlx::query_as(
        r#"
SELECT drv_path, system, required_system_features, is_fod, build_state, output_size, closure_size,
       pname, version, license_json, maintainers_json, meta_position, broken, insecure
FROM Drv
WHERE drv_path = ?
        "#,
    )
    .bind(derivation)
    .fetch_optional(pool)
    .await?;

    Ok(event)
}

pub async fn has_drv(pool: &Pool<Sqlite>, drv_path: &str) -> anyhow::Result<bool> {
    use drv_id::strip_store_path;

    let result = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM Drv WHERE drv_path = $1)")
        .bind(strip_store_path(drv_path))
        .fetch_one(pool)
        .await?;
    Ok(result)
}

/// This will insert a slice of drvs and <referrer, reference> into
/// the database. The reference relationships assume that the drvs
/// have been inserted previously, or passed in this call as well
pub async fn insert_drvs_and_references(
    pool: &Pool<Sqlite>,
    drvs: &[Drv],
    drv_refs: &[(Referrer, Reference)],
) -> anyhow::Result<()> {
    use sqlx::{QueryBuilder, Sqlite};

    if !drvs.is_empty() {
        // Ensure we do not exceed SQLite's 32k limit for query variables.
        // We bind 14 columns per drv now (was 7); 32766 / 14 ~= 2340.
        for drvs_chunk in drvs.chunks(2340) {
            let mut tx = pool.begin().await?;
            let mut query_builder = QueryBuilder::new(
                "INSERT INTO Drv (drv_path, system, required_system_features, is_fod, \
                 build_state, output_size, closure_size, pname, version, license_json, \
                 maintainers_json, meta_position, broken, insecure) ",
            );

            query_builder.push_values(drvs_chunk, |mut row, drv| {
                row.push_bind(&drv.drv_path)
                    .push_bind(&drv.system)
                    .push_bind(&drv.required_system_features)
                    .push_bind(drv.is_fod)
                    .push_bind(&drv.build_state)
                    .push_bind(drv.output_size)
                    .push_bind(drv.closure_size)
                    .push_bind(&drv.pname)
                    .push_bind(&drv.version)
                    .push_bind(&drv.license_json)
                    .push_bind(&drv.maintainers_json)
                    .push_bind(&drv.meta_position)
                    .push_bind(drv.broken)
                    .push_bind(drv.insecure);
            });

            // Override the table-level `UNIQUE (drv_path) ON CONFLICT IGNORE`
            // so that re-evaluations enrich existing rows with metadata
            // without overwriting fields that were previously populated.
            query_builder.push(
                " ON CONFLICT(drv_path) DO UPDATE SET pname = COALESCE(excluded.pname, \
                 Drv.pname), version = COALESCE(excluded.version, Drv.version), license_json = \
                 COALESCE(excluded.license_json, Drv.license_json), maintainers_json = \
                 COALESCE(excluded.maintainers_json, Drv.maintainers_json), meta_position = \
                 COALESCE(excluded.meta_position, Drv.meta_position), broken = \
                 COALESCE(excluded.broken, Drv.broken), insecure = COALESCE(excluded.insecure, \
                 Drv.insecure)",
            );

            query_builder
                .build()
                // Avoid caching queries which are likely to vary greatly in length
                .persistent(false)
                .execute(&mut *tx)
                .await?;
            info!("Inserted {} new drvs", drvs.len());

            tx.commit().await?;
        }
    }

    if !drv_refs.is_empty() {
        // Ensure we do not exceed SQLite's 32k limit for query variables
        // 32766 / 2 ~= 16380
        for refs_chunk in drv_refs.chunks(16380) {
            let mut tx = pool.begin().await?;

            let mut reference_builder: QueryBuilder<Sqlite> =
                QueryBuilder::new("INSERT INTO DrvRefs (referrer, reference) ");
            reference_builder.push_values(refs_chunk, |mut sep, (referrer, reference)| {
                sep.push_bind(referrer).push_bind(reference);
            });

            reference_builder
                .build()
                // Avoid caching queries which are likely to vary greatly in length
                .persistent(false)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
    }

    Ok(())
}

/// To avoid two round trips, or multiple subqueries, we assume that the referrer
/// was recently inserted, thus we know its id. The references will be added
/// by their drv_path since that was not yet known
pub async fn insert_drv_ref(
    pool: &Pool<Sqlite>,
    referrer_id: &DrvId,
    reference_id: &DrvId,
) -> anyhow::Result<()> {
    debug!("Inserting DrvRef ({:?}, {:?})", referrer_id, reference_id);

    sqlx::query(
        r#"
INSERT INTO DrvRefs
    (referrer, reference)
VALUES (?1, ?2)
    "#,
    )
    .bind(referrer_id)
    .bind(reference_id)
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn insert_drv(pool: &Pool<Sqlite>, drv: &Drv) -> anyhow::Result<()> {
    // ON CONFLICT(drv_path) DO UPDATE enriches the existing row with any
    // newly-supplied metadata (COALESCE keeps previously-populated values).
    // The table-level `UNIQUE (drv_path) ON CONFLICT IGNORE` policy is
    // overridden by the explicit ON CONFLICT clause here.
    sqlx::query(
        r#"
INSERT INTO Drv
    (drv_path, system, required_system_features, is_fod, build_state,
     pname, version, license_json, maintainers_json, meta_position, broken, insecure)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
ON CONFLICT(drv_path) DO UPDATE SET
    pname = COALESCE(excluded.pname, Drv.pname),
    version = COALESCE(excluded.version, Drv.version),
    license_json = COALESCE(excluded.license_json, Drv.license_json),
    maintainers_json = COALESCE(excluded.maintainers_json, Drv.maintainers_json),
    meta_position = COALESCE(excluded.meta_position, Drv.meta_position),
    broken = COALESCE(excluded.broken, Drv.broken),
    insecure = COALESCE(excluded.insecure, Drv.insecure)
    "#,
    )
    .bind(&drv.drv_path)
    .bind(&drv.system)
    .bind(&drv.required_system_features)
    .bind(drv.is_fod)
    .bind(DrvBuildState::Queued)
    .bind(&drv.pname)
    .bind(&drv.version)
    .bind(&drv.license_json)
    .bind(&drv.maintainers_json)
    .bind(&drv.meta_position)
    .bind(drv.broken)
    .bind(drv.insecure)
    .execute(pool)
    .await?;

    Ok(())
}

/// Persist package metadata extracted from `nix-eval-jobs --meta` onto
/// pre-existing `Drv` rows. Uses `COALESCE` so a `None` field never blanks
/// out a previously populated value, and silently skips drvs whose row does
/// not (yet) exist in `Drv` — those will be enriched on a future evaluation.
///
/// This is intentionally an UPDATE rather than an UPSERT: at the call site,
/// the FK constraint on `Job.drv_id` already requires the `Drv` row to
/// exist before we can persist a `Job`, so we can rely on it being present.
pub async fn update_drv_package_metadata(
    pool: &Pool<Sqlite>,
    items: &[(DrvId, crate::nix::nix_eval_jobs::DrvPackageMetadata)],
) -> anyhow::Result<()> {
    if items.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;

    for (drv_id, meta) in items {
        // Skip rows where there is genuinely nothing to write — avoids
        // unnecessary DB churn when meta was unavailable.
        if meta.pname.is_none()
            && meta.version.is_none()
            && meta.license_json.is_none()
            && meta.maintainers_json.is_none()
            && meta.meta_position.is_none()
            && meta.broken.is_none()
            && meta.insecure.is_none()
        {
            continue;
        }

        sqlx::query(
            r#"
UPDATE Drv SET
    pname = COALESCE(?1, pname),
    version = COALESCE(?2, version),
    license_json = COALESCE(?3, license_json),
    maintainers_json = COALESCE(?4, maintainers_json),
    meta_position = COALESCE(?5, meta_position),
    broken = COALESCE(?6, broken),
    insecure = COALESCE(?7, insecure)
WHERE drv_path = ?8
            "#,
        )
        .bind(&meta.pname)
        .bind(&meta.version)
        .bind(&meta.license_json)
        .bind(&meta.maintainers_json)
        .bind(&meta.meta_position)
        .bind(meta.broken)
        .bind(meta.insecure)
        .bind(drv_id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

pub async fn update_drv_status(
    pool: &Pool<Sqlite>,
    drv_id: &DrvId,
    build_state: &DrvBuildState,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
UPDATE Drv
SET build_state = ?1
WHERE drv_path = ?2
    "#,
    )
    .bind(build_state)
    .bind(drv_id)
    .execute(pool)
    .await?;

    // TODO: emit build_event in the same transaction
    Ok(())
}

/// "Upstream" drvs, dependencies.
pub async fn drv_references(pool: &Pool<Sqlite>, drv: &DrvId) -> anyhow::Result<Vec<Drv>> {
    let result = sqlx::query_as(
        r#"
SELECT drv_path, system, required_system_features, is_fod, build_state,
       output_size, closure_size,
       pname, version, license_json, maintainers_json, meta_position, broken, insecure
FROM Drv
JOIN DrvRefs ON Drv.drv_path = DrvRefs.reference
WHERE referrer = ?1
"#,
    )
    .bind(drv)
    .fetch_all(pool)
    .await?;

    Ok(result)
}

pub async fn get_derivations_in_state(
    state: DrvBuildState,
    pool: &SqlitePool,
) -> anyhow::Result<Vec<DrvId>> {
    let res = sqlx::query_as(
        r#"
SELECT drv_path
FROM Drv
WHERE build_state = ?
        "#,
    )
    .bind(state)
    .fetch_all(pool)
    .await?;

    Ok(res)
}

/// Get all derivations that can be picked up by builders (Buildable or FailedRetry)
pub async fn get_buildable_and_retry_drvs(pool: &SqlitePool) -> anyhow::Result<Vec<DrvId>> {
    let res = sqlx::query_as(
        r#"
SELECT drv_path
FROM Drv
WHERE build_state = ? OR build_state = ?
        "#,
    )
    .bind(DrvBuildState::Buildable)
    .bind(DrvBuildState::FailedRetry)
    .fetch_all(pool)
    .await?;

    Ok(res)
}

/// Get all transitive referrers (downstream packages) of a drv
/// Uses recursive CTE to find all drvs that transitively depend on the given drv
pub async fn get_all_transitive_referrers(
    drv: &DrvId,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<DrvId>> {
    let result = sqlx::query_as(
        r#"
WITH RECURSIVE transitive_referrers(drv) AS (
    -- Base case: direct referrers
    SELECT referrer FROM DrvRefs WHERE reference = ?1
    UNION
    -- Recursive case: referrers of referrers
    SELECT dr.referrer
    FROM DrvRefs dr
    INNER JOIN transitive_referrers tr ON dr.reference = tr.drv
)
SELECT DISTINCT drv FROM transitive_referrers
"#,
    )
    .bind(drv)
    .fetch_all(pool)
    .await?;

    Ok(result)
}

/// Insert transitive failure records for a failed drv and all its transitive referrers
/// Also updates the build state of each referrer to TransitiveFailure
pub async fn insert_transitive_failures(
    failed_drv: &DrvId,
    transitive_referrers: &[DrvId],
    pool: &Pool<Sqlite>,
) -> anyhow::Result<()> {
    if transitive_referrers.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;

    for referrer in transitive_referrers {
        // Insert the transitive failure record
        sqlx::query("INSERT INTO TransitiveFailure (failed_drv, drv_referrer) VALUES (?, ?)")
            .bind(failed_drv)
            .bind(referrer)
            .execute(&mut *tx)
            .await?;

        // Update the drv's build state to TransitiveFailure
        sqlx::query("UPDATE Drv SET build_state = ? WHERE drv_path = ?")
            .bind(DrvBuildState::TransitiveFailure)
            .bind(referrer)
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Clear all transitive failure records where the given drv was the failure cause
/// This should be called when a previously failed drv is successfully rebuilt
pub async fn clear_transitive_failures(
    failed_drv: &DrvId,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<DrvId>> {
    // First, get all the drvs that were blocked by this failure
    let unblocked_drvs: Vec<DrvId> =
        sqlx::query_as("SELECT drv_referrer FROM TransitiveFailure WHERE failed_drv = ?")
            .bind(failed_drv)
            .fetch_all(pool)
            .await?;

    // Then delete the records
    sqlx::query("DELETE FROM TransitiveFailure WHERE failed_drv = ?")
        .bind(failed_drv)
        .execute(pool)
        .await?;

    Ok(unblocked_drvs)
}

/// Get all failed dependencies that are blocking a given drv
/// Returns the list of failed drvs that this drv transitively depends on
pub async fn get_failed_dependencies(
    drv: &DrvId,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<DrvId>> {
    let result = sqlx::query_as("SELECT failed_drv FROM TransitiveFailure WHERE drv_referrer = ?")
        .bind(drv)
        .fetch_all(pool)
        .await?;

    Ok(result)
}

/// Get all derivations that are in a failed state
/// Returns drvs in Completed(Failure) or TransitiveFailure states
pub async fn get_all_failed_drvs(pool: &Pool<Sqlite>) -> anyhow::Result<Vec<DrvId>> {
    let result = sqlx::query_as(
        "SELECT drv_path FROM Drv WHERE build_state = 'Completed' AND build_result = 'Failure'
         UNION
         SELECT drv_path FROM Drv WHERE build_state = 'TransitiveFailure'",
    )
    .fetch_all(pool)
    .await?;

    Ok(result)
}

use serde::Serialize;
use sqlx::FromRow;

/// Detailed drv information with dependency info
#[derive(Debug, FromRow, Serialize)]
pub struct DrvDetails {
    pub drv_path: DrvId,
    pub system: String,
    pub build_state: DrvBuildState,
    pub is_fod: bool,
    pub required_system_features: Option<String>,
}

/// Get detailed information about a drv (same as get_drv, but as a serializable struct)
pub async fn get_drv_details(
    drv_id: &DrvId,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Option<DrvDetails>> {
    let details = sqlx::query_as(
        r#"
        SELECT drv_path, system, build_state, is_fod, required_system_features
        FROM Drv
        WHERE drv_path = ?
        "#,
    )
    .bind(drv_id)
    .fetch_optional(pool)
    .await?;

    Ok(details)
}

/// Drv dependency information
#[derive(Debug, FromRow, Serialize)]
pub struct DrvDependency {
    pub drv_path: DrvId,
    pub system: String,
    pub build_state: DrvBuildState,
}

/// Get all dependencies of a drv with their build states
pub async fn get_drv_dependencies(
    drv_id: &DrvId,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<DrvDependency>> {
    let deps = sqlx::query_as(
        r#"
        SELECT d.drv_path, d.system, d.build_state
        FROM Drv d
        JOIN DrvRefs dr ON d.drv_path = dr.reference
        WHERE dr.referrer = ?
        ORDER BY d.drv_path
        "#,
    )
    .bind(drv_id)
    .fetch_all(pool)
    .await?;

    Ok(deps)
}

/// Count dependencies
pub async fn count_drv_dependencies(drv_id: &DrvId, pool: &Pool<Sqlite>) -> anyhow::Result<i64> {
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM DrvRefs
        WHERE referrer = ?
        "#,
    )
    .bind(drv_id)
    .fetch_one(pool)
    .await?;

    Ok(count)
}

#[cfg(test)]
mod tests {
    use anyhow::bail;
    use sqlx::SqlitePool;

    use super::super::*;
    use super::*;

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn get_latest_state(pool: SqlitePool) -> anyhow::Result<()> {
        let drv_id = DrvId::dummy();
        let drv = Drv {
            drv_path: drv_id.clone(),
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        insert_drv(&pool, &drv).await?;
        update_drv_status(&pool, &drv_id, &DrvBuildState::Buildable).await?;

        let Some(result) = get_drv(&drv_id, &pool).await? else {
            bail!("Expected query to find a result")
        };

        assert_eq!(result.build_state, DrvBuildState::Buildable);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn insert_many(pool: SqlitePool) -> anyhow::Result<()> {
        let drv1 = Drv {
            drv_path: DrvId::from_str(
                "/nix/store/gciipqhqkdlqqn803zd4a389v86ran45-hello-2.12.1.drv",
            )?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        let drv2 = Drv {
            drv_path: DrvId::from_str(
                "/nix/store/p470qfnbrf16agb4r05fllbsqgi2m8k5-git-2.47.2.drv",
            )?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        let drv3 = Drv {
            drv_path: DrvId::from_str(
                "/nix/store/0wy8117gx1hbdv85x2xq1vf12nlagan4-bash-interactive-5.2p37.drv",
            )?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Buildable,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };

        let drvs = vec![drv1, drv2, drv3];

        insert_drvs_and_references(&pool, &drvs, &Vec::new()).await?;

        let result = get_derivations_in_state(DrvBuildState::Queued, &pool).await?;

        for drv in &result {
            debug!("{:?}", &drv);
        }

        // TODO: make less ugly
        let length = result.len();
        assert_eq!(length, 2);
        Ok(())
    }

    /// End-to-end: an `insert_drv` followed by a metadata-bearing
    /// `update_drv_package_metadata` should populate the new columns and
    /// be readable via `get_drv`.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn metadata_round_trip(pool: SqlitePool) -> anyhow::Result<()> {
        use crate::nix::nix_eval_jobs::DrvPackageMetadata;

        let drv_id =
            DrvId::from_str("/nix/store/gciipqhqkdlqqn803zd4a389v86ran45-hello-2.12.1.drv")?;
        let drv = Drv {
            drv_path: drv_id.clone(),
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        insert_drv(&pool, &drv).await?;

        let meta = DrvPackageMetadata {
            pname: Some("hello".to_string()),
            version: Some("2.12.1".to_string()),
            license_json: Some(r#"[{"shortName":"gpl3Plus"}]"#.to_string()),
            maintainers_json: Some(r#"[{"github":"edolstra"}]"#.to_string()),
            meta_position: Some("pkgs/applications/misc/hello/default.nix:34".to_string()),
            broken: Some(false),
            insecure: Some(false),
        };
        update_drv_package_metadata(&pool, &[(drv_id.clone(), meta)]).await?;

        let result = get_drv(&drv_id, &pool).await?.expect("row present");
        assert_eq!(result.pname.as_deref(), Some("hello"));
        assert_eq!(result.version.as_deref(), Some("2.12.1"));
        assert_eq!(
            result.license_json.as_deref(),
            Some(r#"[{"shortName":"gpl3Plus"}]"#)
        );
        assert_eq!(
            result.maintainers_json.as_deref(),
            Some(r#"[{"github":"edolstra"}]"#)
        );
        assert_eq!(
            result.meta_position.as_deref(),
            Some("pkgs/applications/misc/hello/default.nix:34")
        );
        assert_eq!(result.broken, Some(false));
        assert_eq!(result.insecure, Some(false));
        Ok(())
    }

    /// COALESCE behavior: passing `None` for previously-populated columns
    /// must NOT clear them. We populate metadata once, then call update
    /// again with all-None to confirm the original values survive.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn metadata_coalesce_preserves_previous_values(pool: SqlitePool) -> anyhow::Result<()> {
        use crate::nix::nix_eval_jobs::DrvPackageMetadata;

        let drv_id =
            DrvId::from_str("/nix/store/gciipqhqkdlqqn803zd4a389v86ran45-hello-2.12.1.drv")?;
        let drv = Drv {
            drv_path: drv_id.clone(),
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        insert_drv(&pool, &drv).await?;

        // First update populates pname, version, license.
        update_drv_package_metadata(
            &pool,
            &[(
                drv_id.clone(),
                DrvPackageMetadata {
                    pname: Some("hello".to_string()),
                    version: Some("2.12.1".to_string()),
                    license_json: Some(r#"[{"shortName":"gpl3Plus"}]"#.to_string()),
                    maintainers_json: None,
                    meta_position: None,
                    broken: None,
                    insecure: None,
                },
            )],
        )
        .await?;

        // Second update: all fields None except a new maintainers_json. Must
        // preserve pname/version/license_json from the previous write while
        // adding maintainers_json.
        update_drv_package_metadata(
            &pool,
            &[(
                drv_id.clone(),
                DrvPackageMetadata {
                    pname: None,
                    version: None,
                    license_json: None,
                    maintainers_json: Some(r#"[{"github":"edolstra"}]"#.to_string()),
                    meta_position: None,
                    broken: None,
                    insecure: None,
                },
            )],
        )
        .await?;

        let result = get_drv(&drv_id, &pool).await?.expect("row present");
        assert_eq!(result.pname.as_deref(), Some("hello"));
        assert_eq!(result.version.as_deref(), Some("2.12.1"));
        assert_eq!(
            result.license_json.as_deref(),
            Some(r#"[{"shortName":"gpl3Plus"}]"#)
        );
        assert_eq!(
            result.maintainers_json.as_deref(),
            Some(r#"[{"github":"edolstra"}]"#)
        );
        Ok(())
    }

    /// `update_drv_package_metadata` on a non-existent drv_path must not
    /// error — the UPDATE simply matches zero rows. This is intentional so
    /// the eval-time enrichment is resilient to ordering.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn metadata_update_on_missing_drv_is_noop(pool: SqlitePool) -> anyhow::Result<()> {
        use crate::nix::nix_eval_jobs::DrvPackageMetadata;

        let drv_id =
            DrvId::from_str("/nix/store/gciipqhqkdlqqn803zd4a389v86ran45-hello-2.12.1.drv")?;
        let res = update_drv_package_metadata(
            &pool,
            &[(
                drv_id.clone(),
                DrvPackageMetadata {
                    pname: Some("hello".to_string()),
                    version: Some("2.12.1".to_string()),
                    ..Default::default()
                },
            )],
        )
        .await;
        assert!(
            res.is_ok(),
            "missing-drv update should be a no-op: {:?}",
            res
        );

        // Confirm no row was created (UPDATE doesn't insert).
        assert!(get_drv(&drv_id, &pool).await?.is_none());
        Ok(())
    }
}
