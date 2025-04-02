mod sqlite;

use once_cell::sync::OnceCell;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use crate::error;

static SQLITEPOOL: OnceCell<SqlitePool> = OnceCell::new();

pub async fn initialize(connection: &str) -> error::Result<()> {
    use std::path::Path;

    if SQLITEPOOL.get().is_some() {
        log::debug!("Sqlite already initialized, doing nothing");
        return Ok(());
    }

    log::debug!("Creating SQLite database pool at {}", connection);
    let opts = SqliteConnectOptions::new()
        .filename(connection)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool: SqlitePool = SqlitePool::connect_with(opts).await?;
    SQLITEPOOL.set(pool).expect("DB pool was already set");
    log::debug!("Finished creating SQLite database pool");

    // TODO: allow for path to be set, needs to be available at deployment time
    let migrations_dir = Path::new("./sql/migrations").canonicalize()?;
    log::debug!("Attempting SQL migrations to {}", migrations_dir.display());
    let m = sqlx::migrate::Migrator::new(migrations_dir).await?;
    m.run(pool).await?;

    Ok(())
}

