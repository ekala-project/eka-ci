//! Durable write-ahead log for in-flight service tasks.
//!
//! Each [`TaskJournal`] is scoped to a single service name. Before a
//! task is dispatched to `handle_task`, the serialized payload is
//! persisted to SQLite. After the handler returns `Ok(())`, the row is
//! deleted. On crash-recovery the surviving rows are replayed, giving
//! at-least-once delivery semantics.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use tracing::{debug, info, warn};

/// Opaque identifier for a journaled task row.
#[derive(Debug, Clone, Copy)]
pub struct JournalId(i64);

/// Write-ahead journal for a single service's in-flight tasks.
///
/// `T` must be (de)serializable so it can round-trip through SQLite.
pub struct TaskJournal<T> {
    pool: SqlitePool,
    service_name: String,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> TaskJournal<T>
where
    T: Serialize + for<'de> Deserialize<'de> + std::fmt::Debug,
{
    /// Create a journal scoped to `service_name`.
    pub fn new(pool: SqlitePool, service_name: impl Into<String>) -> Self {
        Self {
            pool,
            service_name: service_name.into(),
            _phantom: std::marker::PhantomData,
        }
    }

    /// Persist `task` before dispatching it. Returns the journal row id
    /// which must be passed to [`acknowledge`] after successful handling.
    pub async fn persist(&self, task: &T) -> Result<JournalId> {
        let payload =
            serde_json::to_string(task).context("failed to serialize task for journal")?;
        let service = &self.service_name;
        let row = sqlx::query_scalar::<_, i64>(
            "INSERT INTO TaskJournal (service, payload) VALUES (?, ?) RETURNING id",
        )
        .bind(service)
        .bind(&payload)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert task into journal")?;

        debug!(
            service = %self.service_name,
            journal_id = row,
            "journaled task"
        );
        Ok(JournalId(row))
    }

    /// Remove a successfully-handled task from the journal.
    pub async fn acknowledge(&self, id: JournalId) -> Result<()> {
        sqlx::query("DELETE FROM TaskJournal WHERE id = ?")
            .bind(id.0)
            .execute(&self.pool)
            .await
            .context("failed to acknowledge journal entry")?;

        debug!(
            service = %self.service_name,
            journal_id = id.0,
            "acknowledged task"
        );
        Ok(())
    }

    /// Recover all un-acknowledged tasks for this service. Called once
    /// at startup to replay any in-flight work that was interrupted.
    pub async fn recover(&self) -> Result<Vec<(JournalId, T)>> {
        let rows = sqlx::query_as::<_, (i64, String)>(
            "SELECT id, payload FROM TaskJournal WHERE service = ? ORDER BY id ASC",
        )
        .bind(&self.service_name)
        .fetch_all(&self.pool)
        .await
        .context("failed to query journal for recovery")?;

        let mut recovered = Vec::with_capacity(rows.len());
        for (id, payload) in rows {
            match serde_json::from_str::<T>(&payload) {
                Ok(task) => {
                    recovered.push((JournalId(id), task));
                },
                Err(e) => {
                    warn!(
                        service = %self.service_name,
                        journal_id = id,
                        error = %e,
                        "failed to deserialize journal entry; deleting corrupt row"
                    );
                    // Remove corrupt entries so they don't block recovery forever.
                    sqlx::query("DELETE FROM TaskJournal WHERE id = ?")
                        .bind(id)
                        .execute(&self.pool)
                        .await
                        .ok();
                },
            }
        }

        if !recovered.is_empty() {
            info!(
                service = %self.service_name,
                count = recovered.len(),
                "recovered un-acknowledged tasks from journal"
            );
        }

        Ok(recovered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbService;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct TestTask {
        name: String,
        value: i32,
    }

    #[tokio::test]
    async fn persist_and_acknowledge_removes_row() {
        let db = DbService::new_in_memory().await.unwrap();
        let journal: TaskJournal<TestTask> = TaskJournal::new(db.pool.clone(), "test-svc");

        let task = TestTask {
            name: "hello".into(),
            value: 42,
        };

        let id = journal.persist(&task).await.unwrap();
        let before = journal.recover().await.unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].1, task);

        journal.acknowledge(id).await.unwrap();
        let after = journal.recover().await.unwrap();
        assert!(after.is_empty());
    }

    #[tokio::test]
    async fn recover_returns_tasks_in_order() {
        let db = DbService::new_in_memory().await.unwrap();
        let journal: TaskJournal<TestTask> = TaskJournal::new(db.pool.clone(), "test-svc");

        let t1 = TestTask {
            name: "first".into(),
            value: 1,
        };
        let t2 = TestTask {
            name: "second".into(),
            value: 2,
        };

        journal.persist(&t1).await.unwrap();
        journal.persist(&t2).await.unwrap();

        let recovered = journal.recover().await.unwrap();
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].1, t1);
        assert_eq!(recovered[1].1, t2);
    }

    #[tokio::test]
    async fn services_are_isolated() {
        let db = DbService::new_in_memory().await.unwrap();
        let j1: TaskJournal<TestTask> = TaskJournal::new(db.pool.clone(), "svc-a");
        let j2: TaskJournal<TestTask> = TaskJournal::new(db.pool.clone(), "svc-b");

        j1.persist(&TestTask {
            name: "a".into(),
            value: 1,
        })
        .await
        .unwrap();
        j2.persist(&TestTask {
            name: "b".into(),
            value: 2,
        })
        .await
        .unwrap();

        let r1 = j1.recover().await.unwrap();
        let r2 = j2.recover().await.unwrap();
        assert_eq!(r1.len(), 1);
        assert_eq!(r2.len(), 1);
        assert_eq!(r1[0].1.name, "a");
        assert_eq!(r2[0].1.name, "b");
    }

    #[tokio::test]
    async fn corrupt_entries_are_cleaned_up() {
        let db = DbService::new_in_memory().await.unwrap();
        let journal: TaskJournal<TestTask> = TaskJournal::new(db.pool.clone(), "test-svc");

        // Insert a corrupt entry directly
        sqlx::query("INSERT INTO TaskJournal (service, payload) VALUES (?, ?)")
            .bind("test-svc")
            .bind("not valid json {{{{")
            .execute(&db.pool)
            .await
            .unwrap();

        let recovered = journal.recover().await.unwrap();
        assert!(recovered.is_empty(), "corrupt entry should be skipped");

        // Verify the corrupt row was deleted
        let remaining =
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM TaskJournal WHERE service = ?")
                .bind("test-svc")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(remaining, 0);
    }
}
