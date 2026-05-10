// Database CRUD for the ChannelPromotion table.
//
// Mirrors the schema landed by `sql/migrations/20260510_release_channels.sql`.
// The table stores promotion *state* keyed by a string `channel_id`;
// channel *configuration* lives in admin-only `ekaci.toml`, so no FK
// constraints are possible. Idempotency and coalescing semantics rely
// on:
//   - the partial UNIQUE index `ChannelPromotionInFlight` (one
//     Evaluating row per channel), and
//   - the index `ChannelPromotionByChannelSha` (fast lookup of any
//     prior decision for a given (channel, sha) pair).

use anyhow::Result;
use sqlx::{FromRow, Pool, Sqlite};

use crate::channels::types::PromotionStatus;

/// One row of `ChannelPromotion`. Columns match the schema exactly.
///
/// Most fields are written more than read today; readers land with
/// the CLI status subcommand (PR 5) and the HTTP audit endpoint (PR 5).
/// Suppressing dead-code keeps the structural contract documented now
/// without polluting the warning surface for unrelated work.
#[allow(dead_code)]
#[derive(Debug, Clone, FromRow)]
pub struct ChannelPromotionRow {
    pub rowid: i64,
    pub channel_id: String,
    pub tracking_sha: String,
    pub target_branch: String,
    pub previous_target_sha: Option<String>,
    pub status: i64,
    pub blocked_reason: Option<String>,
    pub required_results: Option<String>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
}

/// Insert a fresh `Evaluating` row for `(channel_id, tracking_sha)`.
///
/// Returns the new row's `rowid`. Relies on the partial UNIQUE index
/// `ChannelPromotionInFlight` to fail loudly if the caller didn't run
/// the coalescer first.
pub async fn insert_evaluating(
    channel_id: &str,
    tracking_sha: &str,
    target_branch: &str,
    pool: &Pool<Sqlite>,
) -> Result<i64> {
    let started_at = chrono::Utc::now().timestamp();
    let row = sqlx::query(
        r#"
        INSERT INTO ChannelPromotion (
            channel_id, tracking_sha, target_branch,
            previous_target_sha, status,
            blocked_reason, required_results,
            started_at, completed_at
        )
        VALUES (?, ?, ?, NULL, ?, NULL, NULL, ?, NULL)
        RETURNING rowid
        "#,
    )
    .bind(channel_id)
    .bind(tracking_sha)
    .bind(target_branch)
    .bind(PromotionStatus::Evaluating.as_i64())
    .bind(started_at)
    .fetch_one(pool)
    .await?;

    use sqlx::Row;
    Ok(row.try_get("rowid")?)
}

/// Insert a terminal `Skipped` row for a candidate SHA the coalescer
/// pre-empted before any evaluation work was performed.
///
/// Unlike the other terminal helpers, this does not depend on an
/// existing Evaluating row — Skipped rows are pure audit, generated
/// for SHAs that never made it into the in-flight slot.
pub async fn insert_skipped(
    channel_id: &str,
    tracking_sha: &str,
    target_branch: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        r#"
        INSERT INTO ChannelPromotion (
            channel_id, tracking_sha, target_branch,
            previous_target_sha, status,
            blocked_reason, required_results,
            started_at, completed_at
        )
        VALUES (?, ?, ?, NULL, ?, NULL, NULL, ?, ?)
        "#,
    )
    .bind(channel_id)
    .bind(tracking_sha)
    .bind(target_branch)
    .bind(PromotionStatus::Skipped.as_i64())
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Transition the in-flight `Evaluating` row for `(channel_id,
/// tracking_sha)` to a terminal status, recording timestamps and the
/// optional JSON reason/results blobs.
///
/// Only updates rows currently in status=Evaluating, so a stale
/// caller cannot accidentally re-finalise a row that was already
/// concluded by another path.
pub async fn finalise_evaluation(
    channel_id: &str,
    tracking_sha: &str,
    new_status: PromotionStatus,
    blocked_reason_json: Option<&str>,
    required_results_json: Option<&str>,
    previous_target_sha: Option<&str>,
    pool: &Pool<Sqlite>,
) -> Result<u64> {
    debug_assert!(
        !matches!(new_status, PromotionStatus::Evaluating),
        "finalise_evaluation must move to a terminal status"
    );
    let now = chrono::Utc::now().timestamp();
    let res = sqlx::query(
        r#"
        UPDATE ChannelPromotion
        SET status = ?,
            blocked_reason = ?,
            required_results = ?,
            previous_target_sha = ?,
            completed_at = ?
        WHERE channel_id = ?
          AND tracking_sha = ?
          AND status = ?
        "#,
    )
    .bind(new_status.as_i64())
    .bind(blocked_reason_json)
    .bind(required_results_json)
    .bind(previous_target_sha)
    .bind(now)
    .bind(channel_id)
    .bind(tracking_sha)
    .bind(PromotionStatus::Evaluating.as_i64())
    .execute(pool)
    .await?;
    Ok(res.rows_affected())
}

/// Return the current in-flight (`Evaluating`) row for `channel_id`,
/// or `None` if no evaluation is currently running.
///
/// The partial UNIQUE index ensures at most one such row exists.
pub async fn get_in_flight(
    channel_id: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<ChannelPromotionRow>> {
    let row = sqlx::query_as::<_, ChannelPromotionRow>(
        r#"
        SELECT rowid, channel_id, tracking_sha, target_branch,
               previous_target_sha, status, blocked_reason,
               required_results, started_at, completed_at
        FROM ChannelPromotion
        WHERE channel_id = ? AND status = ?
        LIMIT 1
        "#,
    )
    .bind(channel_id)
    .bind(PromotionStatus::Evaluating.as_i64())
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Look up an existing decision (any status) for `(channel_id, sha)`.
///
/// Used by the idempotency guard: a SHA that already has a terminal
/// decision MUST NOT be re-promoted (replayed webhooks would otherwise
/// re-drive the pipeline). The guard treats a single terminal row as
/// sufficient evidence — the most recent one wins if multiple exist
/// (which would only happen if a Skipped row was later succeeded by a
/// proper evaluation, in which case we want the proper one).
pub async fn get_latest_for_sha(
    channel_id: &str,
    sha: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<ChannelPromotionRow>> {
    let row = sqlx::query_as::<_, ChannelPromotionRow>(
        r#"
        SELECT rowid, channel_id, tracking_sha, target_branch,
               previous_target_sha, status, blocked_reason,
               required_results, started_at, completed_at
        FROM ChannelPromotion
        WHERE channel_id = ? AND tracking_sha = ?
        ORDER BY rowid DESC
        LIMIT 1
        "#,
    )
    .bind(channel_id)
    .bind(sha)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Test-only: count rows for a channel by status. Used by the
/// ChannelService unit tests to assert audit-row creation without
/// committing to a particular query API.
#[cfg(test)]
pub async fn count_by_status(
    channel_id: &str,
    status: PromotionStatus,
    pool: &Pool<Sqlite>,
) -> Result<i64> {
    use sqlx::Row;
    let row = sqlx::query(
        r#"
        SELECT COUNT(*) AS n
        FROM ChannelPromotion
        WHERE channel_id = ? AND status = ?
        "#,
    )
    .bind(channel_id)
    .bind(status.as_i64())
    .fetch_one(pool)
    .await?;
    Ok(row.try_get::<i64, _>("n")?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbService;

    fn cid() -> &'static str {
        "github/ekacorp/ekapkgs/stable"
    }

    #[tokio::test]
    async fn insert_and_get_in_flight_roundtrip() {
        let db = DbService::new_in_memory().await.unwrap();
        let rowid = insert_evaluating(cid(), "deadbeef", "ekapkgs-unstable", &db.pool)
            .await
            .unwrap();
        assert!(rowid > 0);

        let in_flight = get_in_flight(cid(), &db.pool).await.unwrap();
        let in_flight = in_flight.expect("Evaluating row missing");
        assert_eq!(in_flight.channel_id, cid());
        assert_eq!(in_flight.tracking_sha, "deadbeef");
        assert_eq!(in_flight.target_branch, "ekapkgs-unstable");
        assert_eq!(in_flight.status, PromotionStatus::Evaluating.as_i64());
        assert!(in_flight.completed_at.is_none());
    }

    #[tokio::test]
    async fn second_evaluating_row_violates_unique_index() {
        // Coalescer's job is to prevent this; verify the schema would
        // catch a buggy caller that bypassed the coalescer.
        let db = DbService::new_in_memory().await.unwrap();
        insert_evaluating(cid(), "sha-a", "tgt", &db.pool)
            .await
            .unwrap();
        let err = insert_evaluating(cid(), "sha-b", "tgt", &db.pool)
            .await
            .err()
            .expect("expected UNIQUE violation");
        let msg = format!("{err:?}");
        assert!(msg.contains("UNIQUE") || msg.contains("constraint"));
    }

    #[tokio::test]
    async fn finalise_only_touches_evaluating_rows() {
        let db = DbService::new_in_memory().await.unwrap();
        insert_evaluating(cid(), "sha-x", "tgt", &db.pool)
            .await
            .unwrap();
        let n = finalise_evaluation(
            cid(),
            "sha-x",
            PromotionStatus::Promoted,
            None,
            Some(r#"{"core":"Success"}"#),
            Some("old-tgt-sha"),
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(n, 1);

        // A second finalise call must NOT find an Evaluating row to update.
        let n2 = finalise_evaluation(
            cid(),
            "sha-x",
            PromotionStatus::PushFailed,
            Some(r#"{"non_ff":true}"#),
            None,
            None,
            &db.pool,
        )
        .await
        .unwrap();
        assert_eq!(n2, 0, "finalise must not re-finalise a terminal row");

        let row = get_latest_for_sha(cid(), "sha-x", &db.pool)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, PromotionStatus::Promoted.as_i64());
        assert!(row.completed_at.is_some());
        assert_eq!(row.previous_target_sha.as_deref(), Some("old-tgt-sha"));
        assert_eq!(
            row.required_results.as_deref(),
            Some(r#"{"core":"Success"}"#)
        );
    }

    #[tokio::test]
    async fn insert_skipped_records_audit_row() {
        let db = DbService::new_in_memory().await.unwrap();
        // Skipped does not require an Evaluating row.
        insert_skipped(cid(), "sha-skipped", "tgt", &db.pool)
            .await
            .unwrap();
        let n = count_by_status(cid(), PromotionStatus::Skipped, &db.pool)
            .await
            .unwrap();
        assert_eq!(n, 1);

        let row = get_latest_for_sha(cid(), "sha-skipped", &db.pool)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, PromotionStatus::Skipped.as_i64());
        assert!(row.completed_at.is_some());
    }

    #[tokio::test]
    async fn get_latest_for_sha_prefers_newest_row() {
        let db = DbService::new_in_memory().await.unwrap();
        // A historical Skipped row followed by a later proper
        // evaluation: get_latest_for_sha should return the newer one.
        insert_skipped(cid(), "sha-q", "tgt", &db.pool)
            .await
            .unwrap();
        insert_evaluating(cid(), "sha-q", "tgt", &db.pool)
            .await
            .unwrap();
        finalise_evaluation(
            cid(),
            "sha-q",
            PromotionStatus::Blocked,
            Some(r#"{"failed_required":["foo"]}"#),
            None,
            None,
            &db.pool,
        )
        .await
        .unwrap();

        let row = get_latest_for_sha(cid(), "sha-q", &db.pool)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, PromotionStatus::Blocked.as_i64());
    }
}
