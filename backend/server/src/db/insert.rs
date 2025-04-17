use anyhow;
use sqlx;
use sqlx::SqlitePool;

/// This is meant to record a drv for building without any associated status
/// Because this does INSERT OR IGNORE the id returned may not be drv inserted,
/// it is assumed that we already checked the drv did not already exist in the db
pub async fn new_drv(drv_path: &str, system: &str, pool: SqlitePool) -> anyhow::Result<i64> {
    let id = sqlx::query("INSERT OR IGNORE INTO Drv (drv_path, platform) VALUES ($1, $2)")
        .bind(drv_path)
        .bind(system)
        .execute(&pool)
        .await?
        .last_insert_rowid();

    Ok(id)
}
