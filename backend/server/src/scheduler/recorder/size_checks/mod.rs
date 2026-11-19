//! Output and closure size checks for successful local builds.
//!
//! [`SizeChecker`] measures a freshly built derivation with one
//! `nix path-info --recursive` call, persists the sizes and compares them
//! against the base-branch baseline of every job it belongs to (see
//! `size_eval`). When a pull request's job has no baseline for a changed
//! attr yet, the base commit's derivation of that attr is measured (see
//! [`baseline`]) and recorded as the baseline first.
//!
//! Every successful build of a job is then reported to its check runs as a
//! [`BuildReport`]: output paths, sizes, build time and the baseline
//! comparisons (see [`report`]). Builds substituted from a cache are
//! measured where their outputs are and reported, but not compared.
//!
//! The check runs in a background task so `nix` calls and substituter lookups
//! never stall the recorder. It is spawned after the recorder sent the plain
//! success update, so the report, which travels the same GitHub batch
//! queue, lands last and is not overwritten.

mod baseline;
mod report;

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::SqlitePool;
use tokio::sync::{OnceCell, Semaphore, mpsc};
use tracing::{debug, warn};

use super::size_eval::{self, SizeComparison};
use crate::db::github::JobInfo;
use crate::db::model::DrvId;
use crate::db::size::SizeMetric;
use crate::github::GitHubTask;
use crate::github::service::build_report::BuildReport;
use crate::nix::size::{OutputSizeReport, measure_outputs};

/// Upper bound on concurrently running size checks (each runs `nix` subprocesses).
const MAX_CONCURRENT_SIZE_CHECKS: usize = 4;

/// Shared, cloneable state for background size checks.
#[derive(Clone)]
pub(super) struct SizeChecker {
    pool: SqlitePool,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    permits: Arc<Semaphore>,
    substituters: Arc<OnceCell<Vec<String>>>,
}

impl SizeChecker {
    pub(super) fn new(pool: SqlitePool, github_sender: Option<mpsc::Sender<GitHubTask>>) -> Self {
        Self {
            pool,
            github_sender,
            permits: Arc::new(Semaphore::new(MAX_CONCURRENT_SIZE_CHECKS)),
            substituters: Arc::new(OnceCell::new()),
        }
    }

    /// Run the size check for `drv` in the background and report the
    /// successful build to its check runs.
    ///
    /// A locally built derivation is measured in the local store, its sizes
    /// are persisted and compared with the base-branch baselines. One
    /// substituted from a cache is measured wherever its outputs are and
    /// only reported, without recording or comparing sizes.
    pub(super) fn spawn(&self, drv: Arc<DrvId>, job_infos: Vec<JobInfo>, built_locally: bool) {
        let checker = self.clone();
        tokio::spawn(async move {
            let Ok(_permit) = checker.permits.acquire().await else {
                return;
            };
            if let Err(e) = checker.run(&drv, &job_infos, built_locally).await {
                warn!("Size check failed for {}: {:#}", drv.store_path(), e);
            }
        });
    }

    async fn run(
        &self,
        drv: &Arc<DrvId>,
        job_infos: &[JobInfo],
        built_locally: bool,
    ) -> anyhow::Result<()> {
        let outputs = match crate::nix::get_drv_outputs(&drv.store_path()).await {
            Ok(outputs) if !outputs.is_empty() => outputs,
            Ok(_) => return Ok(()),
            Err(e) => {
                debug!("No outputs for size check of {}: {}", drv.store_path(), e);
                return Ok(());
            },
        };
        let paths: Vec<String> = outputs.values().cloned().collect();
        let sizes = if built_locally {
            match measure_outputs(&paths, None).await {
                Ok(sizes) => Some(sizes),
                Err(e) => {
                    warn!("Failed to measure {}: {:#}", drv.store_path(), e);
                    None
                },
            }
        } else {
            self.measure_anywhere(&paths).await
        };

        let comparisons = match (&sizes, built_locally) {
            (Some(sizes), true) => {
                self.record_and_compare(drv, job_infos, sizes, &outputs)
                    .await?
            },
            _ => Vec::new(),
        };
        if job_infos.is_empty() {
            // Not a job of any jobset, so there is no check run to report to.
            return Ok(());
        }
        let measured = report::Measured {
            outputs: &outputs,
            sizes: sizes.as_ref(),
            built_locally,
            comparisons,
        };
        let report = report::build_report(&self.pool, drv, measured).await;
        self.send_report(drv, report).await
    }

    /// Persist the measured sizes and compare them with the base-branch
    /// baseline of every job.
    async fn record_and_compare(
        &self,
        drv: &Arc<DrvId>,
        job_infos: &[JobInfo],
        sizes: &OutputSizeReport,
        outputs: &HashMap<String, String>,
    ) -> anyhow::Result<Vec<SizeComparison>> {
        for job_info in job_infos {
            if let Err(e) = self.ensure_base_commit_baseline(job_info).await {
                warn!(
                    "Failed to resolve base-commit baseline of {}: {:#}",
                    job_info.name, e
                );
            }
        }

        let mut comparisons = Vec::new();
        for metric in SizeMetric::ALL {
            let (total, per_output) = metric_sizes(metric, sizes, outputs);
            size_eval::persist_size(&self.pool, drv, metric, total, &per_output).await?;
            comparisons
                .extend(size_eval::evaluate_size(&self.pool, drv, job_infos, metric, total).await?);
        }
        Ok(size_eval::dedupe(comparisons))
    }

    async fn send_report(&self, drv: &Arc<DrvId>, report: BuildReport) -> anyhow::Result<()> {
        let Some(sender) = &self.github_sender else {
            debug!(
                "No GitHub sender; build report for {} not sent",
                drv.store_path()
            );
            return Ok(());
        };
        sender
            .send(GitHubTask::ReportBuildSuccess {
                drv_id: Arc::clone(drv),
                report,
            })
            .await?;
        Ok(())
    }
}

/// The derivation total and per-output-name sizes of `metric`.
///
/// The derivation's closure size is that of all outputs together, so runtime
/// dependencies shared by several outputs are counted once; the per-output
/// closure sizes each include them.
fn metric_sizes(
    metric: SizeMetric,
    report: &OutputSizeReport,
    outputs: &HashMap<String, String>,
) -> (u64, HashMap<String, u64>) {
    let per_output = outputs
        .iter()
        .filter_map(|(name, path)| {
            let size = report.outputs.get(path)?;
            let size = match metric {
                SizeMetric::Output => size.nar_size,
                SizeMetric::Closure => size.closure_size,
            };
            Some((name.clone(), size))
        })
        .collect();
    let total = match metric {
        SizeMetric::Output => report.total_nar_size,
        SizeMetric::Closure => report.total_closure_size,
    };
    (total, per_output)
}
