use std::collections::HashMap;

use anyhow::Result;
use sqlx::SqlitePool;

/// Store runtime references for a specific output of a derivation
///
/// Records which store paths are runtime dependencies (retained in the closure)
/// for a specific output, linked to a derivation by ROWID.
///
/// # Arguments
/// * `pool` - Database connection pool
/// * `drv_id` - ROWID from Drv table
/// * `output_name` - Name of the output (e.g., "out", "dev", "doc")
/// * `output_path` - Full store path of the output
/// * `runtime_refs` - Vector of full store paths that are runtime dependencies
pub async fn store_runtime_references(
    pool: &SqlitePool,
    drv_id: i64,
    output_name: &str,
    output_path: &str,
    runtime_refs: &[String],
) -> Result<()> {
    if runtime_refs.is_empty() {
        return Ok(());
    }

    let mut tx = pool.begin().await?;

    for runtime_ref in runtime_refs {
        sqlx::query(
            r#"
            INSERT INTO DrvRuntimeRefs (drv_id, output_name, output_path, runtime_reference)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(drv_id, output_name, runtime_reference) DO NOTHING
            "#,
        )
        .bind(drv_id)
        .bind(output_name)
        .bind(output_path)
        .bind(runtime_ref)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok(())
}

/// Get runtime references for a specific output
///
/// # Arguments
/// * `pool` - Database connection pool
/// * `drv_id` - ROWID from Drv table
/// * `output_name` - Name of the output (e.g., "out", "dev", "doc")
///
/// # Returns
/// Vector of full store paths that are runtime dependencies
pub async fn get_runtime_references(
    pool: &SqlitePool,
    drv_id: i64,
    output_name: &str,
) -> Result<Vec<String>> {
    let refs = sqlx::query_scalar::<_, String>(
        r#"
        SELECT runtime_reference
        FROM DrvRuntimeRefs
        WHERE drv_id = ?
          AND output_name = ?
        ORDER BY runtime_reference
        "#,
    )
    .bind(drv_id)
    .bind(output_name)
    .fetch_all(pool)
    .await?;

    Ok(refs)
}

/// Get all output paths for a derivation
///
/// # Arguments
/// * `pool` - Database connection pool
/// * `drv_id` - ROWID from Drv table
///
/// # Returns
/// HashMap mapping output names to output paths
pub async fn get_output_paths(pool: &SqlitePool, drv_id: i64) -> Result<HashMap<String, String>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        r#"
        SELECT DISTINCT output_name, output_path
        FROM DrvRuntimeRefs
        WHERE drv_id = ?
        "#,
    )
    .bind(drv_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().collect())
}

/// Update output size for a specific output
///
/// # Arguments
/// * `pool` - Database connection pool
/// * `drv_id` - ROWID from Drv table
/// * `output_name` - Name of the output
/// * `output_size` - Size in bytes
pub async fn update_output_size(
    pool: &SqlitePool,
    drv_id: i64,
    output_name: &str,
    output_size: i64,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE DrvRuntimeRefs
        SET output_size = ?
        WHERE drv_id = ?
          AND output_name = ?
        "#,
    )
    .bind(output_size)
    .bind(drv_id)
    .bind(output_name)
    .execute(pool)
    .await?;

    Ok(())
}

/// Update closure size for a specific output
///
/// # Arguments
/// * `pool` - Database connection pool
/// * `drv_id` - ROWID from Drv table
/// * `output_name` - Name of the output
/// * `closure_size` - Closure size in bytes
pub async fn update_closure_size(
    pool: &SqlitePool,
    drv_id: i64,
    output_name: &str,
    closure_size: i64,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE DrvRuntimeRefs
        SET closure_size = ?
        WHERE drv_id = ?
          AND output_name = ?
        "#,
    )
    .bind(closure_size)
    .bind(drv_id)
    .bind(output_name)
    .execute(pool)
    .await?;

    Ok(())
}
