//! Size-check evaluation against base-branch baselines.
//!
//! The database half of the recorder's size checks: persist a measured output
//! or closure size, record base-branch baselines, and compare the build
//! against the baseline of every job it belongs to. It makes no `nix` calls, so it can be
//! exercised against a real database in tests.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use sqlx::SqlitePool;
use tracing::{debug, info, warn};

use crate::ci::config::{Job, SizeCheck};
use crate::db::github::{JobInfo, JobSetInfo};
use crate::db::model::DrvId;
use crate::db::model::build_event::{DrvBuildResult, DrvBuildState};
use crate::db::size::{self, BaselineKey, SizeMetric};
use crate::github::GitHubTask;

/// A successful build whose output or closure grew beyond its job's
/// threshold.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SizeWarning {
    /// Which size grew.
    pub metric: SizeMetric,
    /// Base branch the baseline was taken from.
    pub base_branch: String,
    /// Baseline size in bytes.
    pub baseline_size: u64,
    /// Size of this build in bytes.
    pub current_size: u64,
    /// Increase over the baseline, in percent.
    pub increase_percent: f64,
    /// Configured `max_increase_percent`.
    pub threshold_percent: f64,
}

impl SizeWarning {
    /// The GitHub task that marks the derivation's check runs neutral with
    /// this warning. Sent in place of the plain success update.
    pub(crate) fn into_github_task(self, drv_id: Arc<DrvId>) -> GitHubTask {
        GitHubTask::UpdateBuildStatusWithSizeWarning {
            drv_id,
            status: DrvBuildState::Completed(DrvBuildResult::Success),
            baseline_size: self.baseline_size,
            current_size: self.current_size,
            increase_percent: self.increase_percent,
            threshold_percent: self.threshold_percent,
            base_branch: self.base_branch,
            metric: self.metric,
        }
    }

    /// The warning with the larger increase, if any.
    pub(crate) fn worse(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(a), Some(b)) if b.increase_percent > a.increase_percent => Some(b),
            (Some(a), _) => Some(a),
            (None, b) => b,
        }
    }
}

/// Compare the `metric` size of `drv_id` against the base-branch baseline of
/// every job it belongs to.
///
/// Before comparing, records the size in the per-commit history and, for
/// jobs in base-branch jobsets, as the new baseline for that attr. Returns
/// the warning with the largest increase, if any job exceeded its threshold.
pub(crate) async fn evaluate_size(
    pool: &SqlitePool,
    drv_id: &DrvId,
    job_infos: &[JobInfo],
    metric: SizeMetric,
    current: u64,
) -> Result<Option<SizeWarning>> {
    let Some(drv) = crate::db::model::drv::get_drv(drv_id, pool).await? else {
        return Ok(None);
    };

    let mut worst: Option<SizeWarning> = None;
    for job_info in job_infos {
        let jobset = match crate::db::github::get_jobset_info(job_info.jobset_id, pool).await {
            Ok(info) => info,
            Err(e) => {
                debug!("No jobset info for jobset {}: {}", job_info.jobset_id, e);
                continue;
            },
        };

        record_history_and_baseline(pool, drv_id, job_info.jobset_id, &jobset, metric, current)
            .await;

        let Some(size_check) = job_size_check(pool, job_info.jobset_id).await? else {
            continue;
        };
        let key = BaselineKey {
            owner: &jobset.owner,
            repo_name: &jobset.repo_name,
            branch: &size_check.base_branch,
            job: &jobset.job,
            attr_path: &job_info.name,
            system: &drv.system,
        };
        let Some(baseline) = size::get_baseline_size(pool, &key, metric).await? else {
            debug!("No baseline {:?} size for {:?}, skipping", metric, key);
            continue;
        };

        let warning = compare_sizes(&size_check, metric, baseline, current);
        log_comparison(drv_id, &key, metric, (baseline, current), warning.is_some());
        worst = SizeWarning::worse(worst, warning);
    }

    Ok(worst)
}

/// Write a measured size to the `Drv` row (`total`) and to the per-output
/// `DrvRuntimeRefs` rows (`per_output`, keyed by output name).
pub(crate) async fn persist_size(
    pool: &SqlitePool,
    drv_id: &DrvId,
    metric: SizeMetric,
    total: u64,
    per_output: &HashMap<String, u64>,
) -> Result<()> {
    use crate::db::runtime_refs::{update_closure_size, update_output_size};

    size::update_drv_size(pool, drv_id, metric, total).await?;

    // Per-output rows exist only once capture_runtime_references has run.
    let Some(drv_rowid) = size::drv_rowid(pool, drv_id).await? else {
        warn!(
            "No Drv row for {}, skipping per-output sizes",
            drv_id.store_path()
        );
        return Ok(());
    };
    for (output_name, size) in per_output {
        let size = *size as i64;
        let result = match metric {
            SizeMetric::Output => update_output_size(pool, drv_rowid, output_name, size).await,
            SizeMetric::Closure => update_closure_size(pool, drv_rowid, output_name, size).await,
        };
        if let Err(e) = result {
            warn!(
                "Failed to update per-output {:?} size for '{}': {}",
                metric, output_name, e
            );
        }
    }

    Ok(())
}

/// Return a warning if `current` exceeds `baseline` by more than the
/// configured percentage.
fn compare_sizes(
    size_check: &SizeCheck,
    metric: SizeMetric,
    baseline: u64,
    current: u64,
) -> Option<SizeWarning> {
    if baseline == 0 {
        return None;
    }
    let increase_percent = (current as f64 - baseline as f64) / baseline as f64 * 100.0;
    (increase_percent > size_check.max_increase_percent).then(|| SizeWarning {
        metric,
        base_branch: size_check.base_branch.clone(),
        baseline_size: baseline,
        current_size: current,
        increase_percent,
        threshold_percent: size_check.max_increase_percent,
    })
}

/// Read the `size_check` section of the job config stored on a jobset.
///
/// `GitHubJobSets.config_json` holds the single job's config (a [`Job`]),
/// not the whole `.ekaci/config.json`.
async fn job_size_check(pool: &SqlitePool, jobset_id: i64) -> Result<Option<SizeCheck>> {
    let config_json: Option<String> =
        sqlx::query_scalar("SELECT config_json FROM GitHubJobSets WHERE ROWID = ?")
            .bind(jobset_id)
            .fetch_optional(pool)
            .await?
            .flatten();
    let Some(config_json) = config_json else {
        return Ok(None);
    };

    match serde_json::from_str::<Job>(&config_json) {
        Ok(job) => Ok(job.size_check),
        Err(e) => {
            warn!(
                "Failed to parse job config of jobset {} for size check: {}",
                jobset_id, e
            );
            Ok(None)
        },
    }
}

fn log_comparison(
    drv_id: &DrvId,
    key: &BaselineKey<'_>,
    metric: SizeMetric,
    (baseline, current): (u64, u64),
    hit: bool,
) {
    let (baseline, current) = (
        crate::nix::size::format_size(baseline),
        crate::nix::size::format_size(current),
    );
    if hit {
        info!(
            "{} threshold exceeded for {} ({} on {}): baseline={}, current={}",
            metric.label(),
            drv_id.store_path(),
            key.attr_path,
            key.branch,
            baseline,
            current
        );
    } else {
        debug!(
            "{} check passed for {} ({} on {}): baseline={}, current={}",
            metric.label(),
            drv_id.store_path(),
            key.attr_path,
            key.branch,
            baseline,
            current
        );
    }
}

/// Record the size in the per-commit history and, if the jobset evaluates a
/// base branch, as that attr's baseline. Failures are logged, not returned:
/// they must not stop the comparison.
async fn record_history_and_baseline(
    pool: &SqlitePool,
    drv_id: &DrvId,
    jobset_id: i64,
    jobset: &JobSetInfo,
    metric: SizeMetric,
    size: u64,
) {
    let git_repo = format!("https://github.com/{}/{}", jobset.owner, jobset.repo_name);
    if let Err(e) =
        size::store_size_history(pool, drv_id, metric, size, &jobset.sha, &git_repo).await
    {
        warn!("Failed to store {:?} size history: {}", metric, e);
    }
    if let Err(e) = size::record_size_baselines(pool, jobset_id, Some(drv_id)).await {
        warn!(
            "Failed to record size baseline for {}: {}",
            drv_id.store_path(),
            e
        );
    }
}

#[cfg(test)]
mod tests;
