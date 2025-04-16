use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use std::sync::OnceLock;
use tracing::debug;

static SQLITEPOOL: OnceLock<SqlitePool> = OnceLock::new();

pub async fn initialize(connection: &str) -> anyhow::Result<()> {
    use std::path::Path;

    if SQLITEPOOL.get().is_some() {
        debug!("Sqlite already initialized, doing nothing");
        return Ok(());
    }

    debug!("Creating SQLite database pool at {}", connection);
    let opts = SqliteConnectOptions::new()
        .filename(connection)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool: SqlitePool = SqlitePool::connect_with(opts).await?;

    // TODO: This shouldn't be a global, rather a struct should wrap
    // the connection pool while exposing a way to do CRUD for most commands
    let _ = SQLITEPOOL.set(pool.clone());
    debug!("Finished creating SQLite database pool");

    // TODO: allow for path to be set, needs to be available at deployment time
    let migrations_dir = Path::new("./sql/migrations").canonicalize()?;
    debug!("Attempting SQL migrations to {}", migrations_dir.display());
    let m = sqlx::migrate::Migrator::new(migrations_dir).await?;
    m.run(&pool).await?;

    Ok(())
}
