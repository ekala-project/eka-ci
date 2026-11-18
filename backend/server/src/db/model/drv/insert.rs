//! Batched inserts of `Drv` rows and their `DrvRefs` edges.

use sqlx::{Pool, QueryBuilder, Sqlite};
use tracing::info;

use super::Drv;
use crate::db::model::{Reference, Referrer};

/// Rows per `Drv` insert. Each row binds 15 columns and SQLite allows at
/// most 32766 variables per statement: 32766 / 15 ~= 2184.
const DRV_CHUNK_SIZE: usize = 2180;

/// Rows per `DrvRefs` insert (2 variables each): 32766 / 2 ~= 16380.
const REF_CHUNK_SIZE: usize = 16380;

/// Override the table-level `UNIQUE (drv_path) ON CONFLICT IGNORE` so that
/// re-evaluations enrich existing rows with metadata without overwriting
/// fields that were previously populated.
///
/// `traversed` only ever goes from 0 to 1. When an eval-only row is
/// traversed, its placeholder build info (system, system features, FOD flag)
/// is replaced by what traversal read from the store.
const DRV_UPSERT: &str = " ON CONFLICT(drv_path) DO UPDATE SET \
    pname = COALESCE(excluded.pname, Drv.pname), \
    version = COALESCE(excluded.version, Drv.version), \
    license_json = COALESCE(excluded.license_json, Drv.license_json), \
    maintainers_json = COALESCE(excluded.maintainers_json, Drv.maintainers_json), \
    meta_position = COALESCE(excluded.meta_position, Drv.meta_position), \
    broken = COALESCE(excluded.broken, Drv.broken), \
    insecure = COALESCE(excluded.insecure, Drv.insecure), \
    system = CASE WHEN Drv.traversed = 0 AND excluded.traversed = 1 \
        THEN excluded.system ELSE Drv.system END, \
    required_system_features = CASE WHEN Drv.traversed = 0 AND excluded.traversed = 1 \
        THEN excluded.required_system_features ELSE Drv.required_system_features END, \
    is_fod = CASE WHEN Drv.traversed = 0 AND excluded.traversed = 1 \
        THEN excluded.is_fod ELSE Drv.is_fod END, \
    traversed = MAX(Drv.traversed, excluded.traversed)";

/// Insert traversed drvs and their `(referrer, reference)` edges.
///
/// The reference relationships assume that the drvs have been inserted
/// previously, or are passed in this call as well.
pub async fn insert_drvs_and_references(
    pool: &Pool<Sqlite>,
    drvs: &[Drv],
    drv_refs: &[(Referrer, Reference)],
) -> anyhow::Result<()> {
    insert_drv_rows(pool, drvs, true).await?;
    insert_drv_refs(pool, drv_refs).await
}

/// Insert job drvs recorded by an evaluation without traversing them.
///
/// These rows exist so `Job` rows can reference them. They are never loaded
/// into the dependency graph, and an existing traversed row stays traversed.
pub async fn insert_eval_only_drvs(pool: &Pool<Sqlite>, drvs: &[Drv]) -> anyhow::Result<()> {
    insert_drv_rows(pool, drvs, false).await
}

async fn insert_drv_rows(pool: &Pool<Sqlite>, drvs: &[Drv], traversed: bool) -> anyhow::Result<()> {
    for drvs_chunk in drvs.chunks(DRV_CHUNK_SIZE) {
        let mut tx = pool.begin().await?;
        let mut query_builder = QueryBuilder::<Sqlite>::new(
            "INSERT INTO Drv (drv_path, system, required_system_features, is_fod, build_state, \
             output_size, closure_size, pname, version, license_json, maintainers_json, \
             meta_position, broken, insecure, traversed) ",
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
                .push_bind(drv.insecure)
                .push_bind(traversed);
        });
        query_builder.push(DRV_UPSERT);

        query_builder
            .build()
            // Avoid caching queries which are likely to vary greatly in length
            .persistent(false)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        info!(
            "Inserted {} drvs (traversed: {})",
            drvs_chunk.len(),
            traversed
        );
    }
    Ok(())
}

async fn insert_drv_refs(
    pool: &Pool<Sqlite>,
    drv_refs: &[(Referrer, Reference)],
) -> anyhow::Result<()> {
    for refs_chunk in drv_refs.chunks(REF_CHUNK_SIZE) {
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
    Ok(())
}
