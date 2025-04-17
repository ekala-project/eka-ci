pub mod insert;

use sqlx::migrate;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use tracing::debug;

#[derive(Clone)]
pub struct DbService {
    conn: SqlitePool,
}

impl DbService {
    pub async fn new(connection: &str) -> anyhow::Result<DbService> {
        use std::path::Path;

        debug!("Creating SQLite database pool at {}", connection);

        // Create the parent directories to the db path if they do not exist
        let db_path_parent = Path::new(connection).parent().expect("Inavlid db path");
        if !db_path_parent.exists() {
            std::fs::create_dir_all(db_path_parent)?;
        }

        let opts = SqliteConnectOptions::new()
            .filename(connection)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool: SqlitePool = SqlitePool::connect_with(opts).await?;
        debug!("Finished creating SQLite database connection pool");

        // TODO: allow for path to be set, needs to be available at deployment time
        debug!("Attempting SQL migrations");
        migrate!("sql/migrations").run(&pool).await?;
        let service = DbService { conn: pool };

        Ok(service)
    }

    #[allow(dead_code)]
    pub async fn insert_drv(&self, drv_path: &str, system: &str) -> anyhow::Result<i64> {
        insert::new_drv(drv_path, system, self.conn.clone()).await
    }
}
