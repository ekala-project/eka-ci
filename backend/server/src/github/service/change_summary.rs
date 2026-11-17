// Change analysis and summary generation

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use octocrab::Octocrab;
use octocrab::models::CheckRunId;
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, warn};

use super::{Commit, GitHubService};
use crate::db::DbService;
use crate::dependency_comparison;
use crate::github::service::{CICheckInfo, actions};
use crate::graph::GraphServiceHandle;
use crate::metrics::ChangeSummaryMetrics;

/// Upper bound on computing and posting one change summary.
const CHANGE_SUMMARY_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Change summaries computed concurrently. They are heavy (graph BFS over
/// every changed drv), so run them one at a time.
const CHANGE_SUMMARY_CONCURRENCY: usize = 1;

/// Computes and posts change-summary checks off the serial GitHubService
/// loop, so a slow or hung summary cannot stall every other GitHub task.
///
/// Summaries are best-effort: the GitHub task is acknowledged once the
/// background job is spawned, so a restart mid-computation drops it.
#[derive(Clone)]
pub(super) struct ChangeSummaryRunner {
    db_service: DbService,
    graph_handle: GraphServiceHandle,
    metrics: Option<Arc<ChangeSummaryMetrics>>,
    /// Change-summary check id per head SHA, for idempotent updates.
    checks: Arc<Mutex<HashMap<Commit, CheckRunId>>>,
    permits: Arc<Semaphore>,
}

impl ChangeSummaryRunner {
    pub(super) fn new(
        db_service: DbService,
        graph_handle: GraphServiceHandle,
        metrics: Option<Arc<ChangeSummaryMetrics>>,
    ) -> Self {
        Self {
            db_service,
            graph_handle,
            metrics,
            checks: Arc::new(Mutex::new(HashMap::new())),
            permits: Arc::new(Semaphore::new(CHANGE_SUMMARY_CONCURRENCY)),
        }
    }

    /// Spawn the summary for `ci_check_info` in the background.
    fn spawn(&self, octocrab: Octocrab, ci_check_info: Arc<CICheckInfo>, job: String) {
        let runner = self.clone();
        crate::services::spawn_logged("change-summary", async move {
            let commit = ci_check_info.commit.clone();
            let work = runner.run(&octocrab, &ci_check_info, &job);
            if let Err(e) = run_bounded(&runner.permits, CHANGE_SUMMARY_TIMEOUT, work).await {
                warn!("Change-summary check for {} failed: {:?}", commit, e);
            }
        });
    }

    async fn run(&self, octocrab: &Octocrab, ci_check_info: &CICheckInfo, job: &str) -> Result<()> {
        let Some(markdown) = self.build_markdown(ci_check_info, job).await? else {
            return Ok(());
        };
        self.post(octocrab, ci_check_info, markdown).await
    }

    /// Build the summary markdown; `None` when there is nothing to post.
    async fn build_markdown(
        &self,
        ci_check_info: &CICheckInfo,
        job: &str,
    ) -> Result<Option<String>> {
        let Some(base_sha) = ci_check_info.base_commit.as_deref() else {
            debug!(
                "Skipping change-summary check for {}: no base commit (not a PR head)",
                &ci_check_info.commit
            );
            return Ok(None);
        };
        let pool = &self.db_service.pool;
        let Some((head_jobset_id, owner, repo_name)) =
            load_head_jobset(pool, &ci_check_info.commit, job).await?
        else {
            debug!(
                "No head jobset for sha={} job={}; skipping change-summary check",
                &ci_check_info.commit, job
            );
            return Ok(None);
        };
        let base_jobset_id: Option<i64> =
            sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
                .bind(base_sha)
                .bind(job)
                .fetch_optional(pool)
                .await?;

        let jobset_data = crate::JobsetData::new(
            owner,
            repo_name,
            "github.com",
            &ci_check_info.commit,
            job,
            None, // config_json not needed for change summary
        );
        self.summarize(head_jobset_id, base_jobset_id, &jobset_data, base_sha)
            .await
            .map(Some)
    }

    /// Render the change-summary markdown for a resolved head/base jobset.
    async fn summarize(
        &self,
        head_jobset_id: i64,
        base_jobset_id: Option<i64>,
        jobset_data: &crate::JobsetData,
        base_sha: &str,
    ) -> Result<String> {
        let (opts, status) = crate::change_summary_compat::resolve_options_from_jobset_data(
            jobset_data,
            self.metrics.as_deref(),
        )
        .await;
        let summary = crate::change_summary::build_change_summary_from_jobset_ids(
            &self.db_service.pool,
            &self.graph_handle,
            head_jobset_id,
            base_jobset_id,
            jobset_data,
            base_sha,
            &opts,
            &status,
            None, // metrics not supported yet
        )
        .await
        .context("failed to build change summary")?;
        Ok(summary.markdown)
    }

    /// Create the change-summary check, or update the one already posted.
    async fn post(
        &self,
        octocrab: &Octocrab,
        ci_check_info: &CICheckInfo,
        markdown: String,
    ) -> Result<()> {
        let existing = self.checks.lock().await.get(&ci_check_info.commit).copied();
        if let Some(existing) = existing {
            return actions::update_change_summary_check(
                octocrab,
                ci_check_info,
                existing,
                markdown,
            )
            .await
            .with_context(|| format!("failed to update change-summary check {existing}"));
        }
        let check_run = actions::create_change_summary_check(octocrab, ci_check_info, markdown)
            .await
            .context("failed to create change-summary check")?;
        self.checks
            .lock()
            .await
            .insert(ci_check_info.commit.clone(), check_run.id);
        Ok(())
    }
}

/// The head jobset's `(ROWID, owner, repo_name)` for `sha`/`job`.
async fn load_head_jobset(
    pool: &sqlx::SqlitePool,
    sha: &str,
    job: &str,
) -> Result<Option<(i64, String, String)>> {
    let row = sqlx::query_as(
        "SELECT ROWID, owner, repo_name FROM GitHubJobSets WHERE sha = ? AND job = ?",
    )
    .bind(sha)
    .bind(job)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Run `work` holding one of `permits`, giving up after `timeout`
/// (time spent waiting for a permit counts towards it).
async fn run_bounded<F>(permits: &Semaphore, timeout: Duration, work: F) -> Result<()>
where
    F: Future<Output = Result<()>>,
{
    let bounded = async {
        let _permit = permits
            .acquire()
            .await
            .context("change-summary semaphore closed")?;
        work.await
    };
    tokio::time::timeout(timeout, bounded)
        .await
        .with_context(|| format!("timed out after {timeout:?}"))?
}

impl GitHubService {
    pub(super) async fn handle_create_dependency_changes_gate(
        &self,
        ci_check_info: &CICheckInfo,
        jobset_id: i64,
        base_jobset_id: i64,
    ) -> Result<()> {
        let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

        debug!(
            "Creating dependency changes gate for commit {} (jobset: {}, base: {})",
            &ci_check_info.commit, jobset_id, base_jobset_id
        );

        let comparisons = dependency_comparison::compare_runtime_references_for_jobset(
            base_jobset_id,
            jobset_id,
            &self.db_service.pool,
        )
        .await?;

        let dependency_diff =
            dependency_comparison::format_dependency_changes_as_diff(&comparisons);

        actions::create_dependency_changes_gate(
            &octocrab,
            ci_check_info,
            &dependency_diff,
            comparisons.len(),
        )
        .await?;

        debug!(
            "Successfully created dependency changes gate with {} packages affected",
            comparisons.len()
        );
        Ok(())
    }

    /// Hand the aggregated change-summary check for a head SHA to the
    /// background [`ChangeSummaryRunner`]; returns without waiting for it.
    pub(super) async fn handle_create_change_summary_check(
        &self,
        ci_check_info: &Arc<CICheckInfo>,
        job: &str,
    ) -> Result<()> {
        self.change_summary_pending
            .lock()
            .await
            .remove(&ci_check_info.commit);
        let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
        self.change_summary
            .spawn(octocrab, Arc::clone(ci_check_info), job.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_bounded_times_out_and_releases_permit() {
        let permits = Semaphore::new(1);
        let hung = std::future::pending::<Result<()>>();
        let result = run_bounded(&permits, Duration::from_millis(20), hung).await;
        assert!(result.is_err(), "hung work must time out");
        assert_eq!(permits.available_permits(), 1, "permit must be released");
    }

    #[tokio::test]
    async fn run_bounded_serializes_work() {
        let permits = Semaphore::new(1);
        let held = permits.acquire().await.expect("acquire");
        let result = run_bounded(&permits, Duration::from_millis(20), async { Ok(()) }).await;
        assert!(result.is_err(), "work must wait for the held permit");
        drop(held);
        run_bounded(&permits, Duration::from_secs(1), async { Ok(()) })
            .await
            .expect("work runs once the permit is free");
    }
}
