use crate::db;
use sqlx;

pub async fn new(drv_path: &str) {
    // TODO: allow for platform to be handled correctly
    sqlx::query("INSERT OR IGNORE INTO Drv (drv_path, platform) VALUES ($1, 'x86_64-linux')")
        .bind(drv_path)
        .execute(db::SQLITEPOOL.get().unwrap())
        .await.unwrap();
}
