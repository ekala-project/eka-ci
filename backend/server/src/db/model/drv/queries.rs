use sqlx::{Pool, Sqlite, SqlitePool};
use tracing::{debug, info};

use super::Drv;
use crate::db::model::build_event::DrvBuildState;
use crate::db::model::{DrvId, Reference, Referrer, drv_id};

pub async fn get_drv(derivation: &DrvId, pool: &Pool<Sqlite>) -> anyhow::Result<Option<Drv>> {
    let event = sqlx::query_as(
        r#"
SELECT drv_path, system, required_system_features, build_state
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
        // Ensure we do not exceed SQLite's 32k limit for query variables
        // 32766 / 4 ~= 8190
        for drvs_chunk in drvs.chunks(8190) {
            let mut tx = pool.begin().await?;
            let mut query_builder = QueryBuilder::new(
                "INSERT INTO Drv (drv_path, system, required_system_features, build_state) ",
            );

            query_builder.push_values(drvs_chunk, |mut row, drv| {
                row.push_bind(&drv.drv_path)
                    .push_bind(&drv.system)
                    .push_bind(&drv.required_system_features)
                    .push_bind(&drv.build_state);
            });

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
    sqlx::query(
        r#"
INSERT INTO Drv
    (drv_path, system, required_system_features, build_state)
VALUES (?1, ?2, ?3, ?4)
    "#,
    )
    .bind(&drv.drv_path)
    .bind(&drv.system)
    .bind(&drv.required_system_features)
    .bind(DrvBuildState::Queued)
    .execute(pool)
    .await?;

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
SELECT drv_path, system, required_system_features, build_state
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

/// "Downstream" drvs, consumers.
pub async fn drv_referrers(pool: &Pool<Sqlite>, drv: &DrvId) -> anyhow::Result<Vec<DrvId>> {
    let result = sqlx::query_as(
        r#"
SELECT referrer
FROM DrvRefs
WHERE reference = ?1
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
            build_state: DrvBuildState::Queued,
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
            build_state: DrvBuildState::Queued,
        };
        let drv2 = Drv {
            drv_path: DrvId::from_str(
                "/nix/store/p470qfnbrf16agb4r05fllbsqgi2m8k5-git-2.47.2.drv",
            )?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            build_state: DrvBuildState::Queued,
        };
        let drv3 = Drv {
            drv_path: DrvId::from_str(
                "/nix/store/0wy8117gx1hbdv85x2xq1vf12nlagan4-bash-interactive-5.2p37.drv",
            )?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            build_state: DrvBuildState::Buildable,
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
}
