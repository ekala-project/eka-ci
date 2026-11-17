//! Evaluation requests for pull-request head commits.
//!
//! A PR is identified for evaluation purposes by its head commit SHA. A
//! row here records that an evaluation of that commit was queued; the
//! PR reconciler (`github::reconciler`) uses it, together with
//! `GitHubJobSets`, to decide whether the commit still needs evaluating.

use anyhow::Result;
use sqlx::{FromRow, Pool, Sqlite};

/// State of the evaluation request for one head commit.
#[derive(Debug, Clone, PartialEq, Eq, FromRow)]
pub struct EvalRequest {
    pub requested_at: i64,
    pub attempts: i64,
    pub completed_at: Option<i64>,
}

/// Identifies a head commit within a repository.
#[derive(Debug, Clone, Copy)]
pub struct HeadCommit<'a> {
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub sha: &'a str,
}

pub async fn get_eval_request(
    head: HeadCommit<'_>,
    pool: &Pool<Sqlite>,
) -> Result<Option<EvalRequest>> {
    let row = sqlx::query_as(
        "SELECT requested_at, attempts, completed_at FROM GitHubPrEvalRequests
         WHERE owner = ? AND repo_name = ? AND head_sha = ?",
    )
    .bind(head.owner)
    .bind(head.repo_name)
    .bind(head.sha)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Record that an evaluation of `head` was queued.
///
/// `from_reconciler` counts the request against the reconciler's retry
/// budget; webhook-driven requests don't. Either way the request is
/// re-opened (`completed_at` cleared).
pub async fn record_eval_request(
    head: HeadCommit<'_>,
    pr_number: i64,
    from_reconciler: bool,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let increment = i64::from(from_reconciler);
    sqlx::query(
        "INSERT INTO GitHubPrEvalRequests (owner, repo_name, head_sha, pr_number, attempts)
         VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (owner, repo_name, head_sha) DO UPDATE SET
             pr_number = excluded.pr_number,
             requested_at = unixepoch(),
             attempts = attempts + excluded.attempts,
             completed_at = NULL",
    )
    .bind(head.owner)
    .bind(head.repo_name)
    .bind(head.sha)
    .bind(pr_number)
    .bind(increment)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark the evaluation of `head` as finished without a jobset (no jobs
/// configured, or the eval failed), so it is not requested again.
pub async fn mark_eval_request_completed(head: HeadCommit<'_>, pool: &Pool<Sqlite>) -> Result<()> {
    sqlx::query(
        "UPDATE GitHubPrEvalRequests SET completed_at = unixepoch()
         WHERE owner = ? AND repo_name = ? AND head_sha = ?",
    )
    .bind(head.owner)
    .bind(head.repo_name)
    .bind(head.sha)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbService;

    const HEAD: HeadCommit<'static> = HeadCommit {
        owner: "o",
        repo_name: "r",
        sha: "abc",
    };

    #[tokio::test]
    async fn webhook_requests_do_not_consume_attempts() {
        let db = DbService::new_in_memory().await.unwrap();
        record_eval_request(HEAD, 7, false, &db.pool).await.unwrap();
        record_eval_request(HEAD, 7, false, &db.pool).await.unwrap();
        let req = get_eval_request(HEAD, &db.pool).await.unwrap().unwrap();
        assert_eq!(req.attempts, 0);
        assert_eq!(req.completed_at, None);
    }

    #[tokio::test]
    async fn reconciler_requests_count_and_reopen() {
        let db = DbService::new_in_memory().await.unwrap();
        record_eval_request(HEAD, 7, true, &db.pool).await.unwrap();
        mark_eval_request_completed(HEAD, &db.pool).await.unwrap();
        let req = get_eval_request(HEAD, &db.pool).await.unwrap().unwrap();
        assert!(req.completed_at.is_some());

        record_eval_request(HEAD, 7, true, &db.pool).await.unwrap();
        let req = get_eval_request(HEAD, &db.pool).await.unwrap().unwrap();
        assert_eq!(req.attempts, 2);
        assert_eq!(req.completed_at, None);
    }

    #[tokio::test]
    async fn unknown_head_has_no_request() {
        let db = DbService::new_in_memory().await.unwrap();
        assert!(get_eval_request(HEAD, &db.pool).await.unwrap().is_none());
    }
}
