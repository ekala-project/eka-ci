//! Size-check evaluation against base-branch baselines.
//!
//! The database half of the recorder's size checks: persist a measured output
//! size, record base-branch baselines, and compare the build against the
//! baseline of every job it belongs to. It makes no `nix` calls, so it can be
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
use crate::db::size::{self, BaselineKey};
use crate::github::GitHubTask;

/// A successful build whose output grew beyond its job's threshold.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SizeWarning {
    /// Base branch the baseline was taken from.
    pub base_branch: String,
    /// Baseline output size in bytes.
    pub baseline_size: u64,
    /// Output size of this build in bytes.
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
        }
    }
}

/// Compare every job `drv_id` belongs to against its base-branch baseline.
///
/// Before comparing, records the size in the per-commit history and, for
/// jobs in base-branch jobsets, as the new baseline for that attr. Returns
/// the warning with the largest increase, if any job exceeded its threshold.
pub(crate) async fn evaluate_output_size(
    pool: &SqlitePool,
    drv_id: &DrvId,
    job_infos: &[JobInfo],
    output_size: u64,
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

        record_history_and_baseline(pool, drv_id, job_info.jobset_id, &jobset, output_size).await;

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
        let Some(baseline_size) = size::get_baseline_output_size(pool, &key).await? else {
            debug!("No baseline output size for {:?}, skipping size check", key);
            continue;
        };

        let warning = compare_sizes(&size_check, baseline_size, output_size);
        log_comparison(drv_id, &key, baseline_size, output_size, warning.is_some());
        if let Some(warning) = warning {
            if worst
                .as_ref()
                .is_none_or(|w| warning.increase_percent > w.increase_percent)
            {
                worst = Some(warning);
            }
        }
    }

    Ok(worst)
}

/// Write the measured output size to the `Drv` row and to the per-output
/// `DrvRuntimeRefs` rows. Returns the total across all outputs.
pub(crate) async fn persist_output_size(
    pool: &SqlitePool,
    drv_id: &DrvId,
    outputs: &HashMap<String, String>,
    sizes_by_path: &HashMap<String, u64>,
) -> Result<u64> {
    let output_size: u64 = sizes_by_path.values().sum();

    size::update_drv_output_size(pool, drv_id, output_size).await?;

    // Per-output rows exist only once capture_runtime_references has run.
    let Some(drv_rowid) = size::drv_rowid(pool, drv_id).await? else {
        warn!(
            "No Drv row for {}, skipping per-output sizes",
            drv_id.store_path()
        );
        return Ok(output_size);
    };
    for (output_name, output_path) in outputs {
        let Some(size) = sizes_by_path.get(output_path) else {
            continue;
        };
        if let Err(e) =
            crate::db::runtime_refs::update_output_size(pool, drv_rowid, output_name, *size as i64)
                .await
        {
            warn!(
                "Failed to update per-output size for '{}' ({}): {}",
                output_name, output_path, e
            );
        }
    }

    Ok(output_size)
}

/// Return a warning if `current` exceeds `baseline` by more than the
/// configured percentage.
fn compare_sizes(size_check: &SizeCheck, baseline: u64, current: u64) -> Option<SizeWarning> {
    if baseline == 0 {
        return None;
    }
    let increase_percent = (current as f64 - baseline as f64) / baseline as f64 * 100.0;
    (increase_percent > size_check.max_increase_percent).then(|| SizeWarning {
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

fn log_comparison(drv_id: &DrvId, key: &BaselineKey<'_>, baseline: u64, current: u64, hit: bool) {
    let (baseline, current) = (
        crate::nix::size::format_size(baseline),
        crate::nix::size::format_size(current),
    );
    if hit {
        info!(
            "Output size threshold exceeded for {} ({} on {}): baseline={}, current={}",
            drv_id.store_path(),
            key.attr_path,
            key.branch,
            baseline,
            current
        );
    } else {
        debug!(
            "Output size check passed for {} ({} on {}): baseline={}, current={}",
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
    output_size: u64,
) {
    let git_repo = format!("https://github.com/{}/{}", jobset.owner, jobset.repo_name);
    if let Err(e) = size::store_output_size(pool, drv_id, output_size, &jobset.sha, &git_repo).await
    {
        warn!("Failed to store output size history: {}", e);
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
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::db::DbService;
    use crate::nix::NixEvalDrv;

    const BASE_DRV: &str = "/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv";
    const HEAD_DRV: &str = "/nix/store/gciipqhqkdlqqn803zd4a389v86ran45-hello-2.12.2.drv";
    const JOB_CONFIG: &str = r#"{"file":"default.nix","size_check":{"max_increase_percent":10.0,"base_branch":"master"}}"#;

    fn hello(drv_path: &str) -> NixEvalDrv {
        NixEvalDrv {
            attr: "hello".to_string(),
            attr_path: vec!["hello".to_string()],
            drv_path: drv_path.to_string(),
            input_drvs: Default::default(),
            name: "hello".to_string(),
            outputs: Default::default(),
            system: "x86_64-linux".to_string(),
            meta: None,
            pos: None,
        }
    }

    /// Create the base jobset for `master` and the head jobset of a PR that
    /// changes `hello`, the way the GitHub service does.
    async fn base_and_head(db: &DbService) -> Result<(i64, i64)> {
        let jobs = [hello(BASE_DRV)];
        let base = db
            .create_github_jobset_with_jobs(
                "base-sha",
                "packages",
                "owner",
                "repo",
                &jobs,
                Some(JOB_CONFIG),
                None,
            )
            .await?;
        size::set_jobset_branch(&db.pool, base, "master").await?;

        let jobs = [hello(HEAD_DRV)];
        let head = db
            .create_github_jobset_with_jobs(
                "head-sha",
                "packages",
                "owner",
                "repo",
                &jobs,
                Some(JOB_CONFIG),
                Some("base-sha"),
            )
            .await?;
        Ok((base, head))
    }

    /// Record a successful build of `drv_path` with one output of `size` bytes
    /// and run the size check, as the recorder does after `nix path-info`.
    async fn build(db: &DbService, drv_path: &str, size: u64) -> Result<Option<SizeWarning>> {
        let drv_id = DrvId::from_str(drv_path)?;
        let out = format!("/nix/store/{}-out", drv_id.drv_hash());
        let outputs = HashMap::from([("out".to_string(), out.clone())]);
        let sizes = HashMap::from([(out, size)]);
        let total = persist_output_size(&db.pool, &drv_id, &outputs, &sizes).await?;
        let job_infos = db.get_job_info_for_drv(&drv_id).await?;
        evaluate_output_size(&db.pool, &drv_id, &job_infos, total).await
    }

    /// End to end: the base derivation is built (as a PR head before it was
    /// merged, or as a base-branch build), the PR's new derivation of the same
    /// attr is 50% larger, and the warning reaches the GitHub task.
    #[tokio::test]
    async fn warning_fires_against_base_branch_baseline() -> Result<()> {
        let db = DbService::new_in_memory().await?;
        let (_base, head) = base_and_head(&db).await?;

        // Building a drv of a base-branch jobset records the baseline.
        assert_eq!(build(&db, BASE_DRV, 1_000_000).await?, None);

        let warning = build(&db, HEAD_DRV, 1_500_000)
            .await?
            .expect("50% increase exceeds the 10% threshold");
        assert_eq!(warning.base_branch, "master");
        assert_eq!(warning.baseline_size, 1_000_000);
        assert_eq!(warning.current_size, 1_500_000);
        assert!((warning.increase_percent - 50.0).abs() < 1e-9);
        assert_eq!(warning.threshold_percent, 10.0);

        let drv_id = Arc::new(DrvId::from_str(HEAD_DRV)?);
        match warning.into_github_task(Arc::clone(&drv_id)) {
            GitHubTask::UpdateBuildStatusWithSizeWarning {
                drv_id: task_drv,
                status,
                baseline_size,
                current_size,
                base_branch,
                ..
            } => {
                assert_eq!(task_drv, drv_id);
                assert_eq!(status, DrvBuildState::Completed(DrvBuildResult::Success));
                assert_eq!((baseline_size, current_size), (1_000_000, 1_500_000));
                assert_eq!(base_branch, "master");
            },
            other => panic!("unexpected task {:?}", other),
        }

        // The head jobset is not a base-branch jobset: no baseline from it.
        let branch: Option<String> =
            sqlx::query_scalar("SELECT branch FROM GitHubJobSets WHERE ROWID = ?")
                .bind(head)
                .fetch_one(&db.pool)
                .await?;
        assert_eq!(branch, None);
        Ok(())
    }

    /// The common case: the base derivation was built before the base jobset
    /// existed, so the baseline is seeded when the jobset is created.
    #[tokio::test]
    async fn baseline_seeded_from_earlier_build() -> Result<()> {
        let db = DbService::new_in_memory().await?;
        let base_id = DrvId::from_str(BASE_DRV)?;
        let (base, _head) = base_and_head(&db).await?;
        size::update_drv_output_size(&db.pool, &base_id, 1_000_000).await?;
        assert_eq!(size::record_size_baselines(&db.pool, base, None).await?, 1);

        assert!(build(&db, HEAD_DRV, 1_200_000).await?.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn no_warning_within_threshold_or_without_baseline() -> Result<()> {
        let db = DbService::new_in_memory().await?;
        base_and_head(&db).await?;

        // No base build yet: nothing to compare against.
        assert_eq!(build(&db, HEAD_DRV, 1_500_000).await?, None);

        build(&db, BASE_DRV, 1_000_000).await?;
        assert_eq!(build(&db, HEAD_DRV, 1_050_000).await?, None);
        Ok(())
    }

    /// `GitHubJobSets.config_json` holds a single job's config. Parsing it as
    /// the whole `.ekaci/config.json` (as the recorder used to) silently
    /// yields no jobs, so the size check never ran.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn job_size_check_reads_single_job_config(pool: SqlitePool) -> Result<()> {
        let jobset = crate::db::github::create_jobset(
            "sha",
            "packages",
            "owner",
            "repo",
            Some(JOB_CONFIG),
            &pool,
        )
        .await?;

        let as_whole_config: crate::ci::config::CIConfig = serde_json::from_str(JOB_CONFIG)?;
        assert!(as_whole_config.jobs.is_empty());

        let size_check = job_size_check(&pool, jobset)
            .await?
            .expect("size_check is configured");
        assert_eq!(size_check.base_branch, "master");
        assert_eq!(size_check.max_increase_percent, 10.0);
        Ok(())
    }

    #[test]
    fn compare_sizes_threshold() {
        let size_check = SizeCheck {
            max_increase_percent: 10.0,
            base_branch: "main".to_string(),
        };
        assert!(compare_sizes(&size_check, 100, 110).is_none());
        assert!(compare_sizes(&size_check, 100, 111).is_some());
        assert!(compare_sizes(&size_check, 100, 50).is_none());
        assert!(compare_sizes(&size_check, 0, 100).is_none());
    }
}
