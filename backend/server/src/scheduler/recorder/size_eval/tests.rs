//! Tests for size-check evaluation against base-branch baselines.

use std::str::FromStr;

use super::*;
use crate::db::DbService;
use crate::nix::NixEvalDrv;

const BASE_DRV: &str = "/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv";
const HEAD_DRV: &str = "/nix/store/gciipqhqkdlqqn803zd4a389v86ran45-hello-2.12.2.drv";
const JOB_CONFIG: &str =
    r#"{"file":"default.nix","size_check":{"max_increase_percent":10.0,"base_branch":"master"}}"#;

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

/// Record the `metric` size of a successful build of `drv_path` and run
/// the size check, as the recorder does after `nix path-info`.
async fn measure(
    db: &DbService,
    drv_path: &str,
    metric: SizeMetric,
    size: u64,
) -> Result<Option<SizeWarning>> {
    let drv_id = DrvId::from_str(drv_path)?;
    let per_output = HashMap::from([("out".to_string(), size)]);
    persist_size(&db.pool, &drv_id, metric, size, &per_output).await?;
    let job_infos = db.get_job_info_for_drv(&drv_id).await?;
    evaluate_size(&db.pool, &drv_id, &job_infos, metric, size).await
}

async fn build(db: &DbService, drv_path: &str, size: u64) -> Result<Option<SizeWarning>> {
    measure(db, drv_path, SizeMetric::Output, size).await
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
    assert_eq!(warning.metric, SizeMetric::Output);
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

/// Closure sizes are compared on the same baseline as output sizes, and a
/// closure warning names its metric in the GitHub task.
#[tokio::test]
async fn closure_warning_fires_against_base_branch_baseline() -> Result<()> {
    let db = DbService::new_in_memory().await?;
    base_and_head(&db).await?;
    let closure = SizeMetric::Closure;

    build(&db, BASE_DRV, 1_000).await?;
    measure(&db, BASE_DRV, closure, 1_000_000).await?;

    // The output did not grow; its closure doubled.
    assert_eq!(build(&db, HEAD_DRV, 1_000).await?, None);
    let warning = measure(&db, HEAD_DRV, closure, 2_000_000)
        .await?
        .expect("100% closure increase exceeds the 10% threshold");
    assert_eq!(warning.metric, closure);
    assert_eq!(warning.baseline_size, 1_000_000);

    let drv_id = Arc::new(DrvId::from_str(HEAD_DRV)?);
    match warning.into_github_task(drv_id) {
        GitHubTask::UpdateBuildStatusWithSizeWarning { metric, .. } => {
            assert_eq!(metric, closure)
        },
        other => panic!("unexpected task {:?}", other),
    }

    let closure_size: Option<i64> =
        sqlx::query_scalar("SELECT closure_size FROM Drv WHERE drv_path = ?")
            .bind(DrvId::from_str(HEAD_DRV)?)
            .fetch_one(&db.pool)
            .await?;
    assert_eq!(closure_size, Some(2_000_000));
    Ok(())
}

/// The common case: the base derivation was built before the base jobset
/// existed, so the baseline is seeded when the jobset is created.
#[tokio::test]
async fn baseline_seeded_from_earlier_build() -> Result<()> {
    let db = DbService::new_in_memory().await?;
    let base_id = DrvId::from_str(BASE_DRV)?;
    let (base, _head) = base_and_head(&db).await?;
    size::update_drv_size(&db.pool, &base_id, SizeMetric::Output, 1_000_000).await?;
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
    let out = SizeMetric::Output;
    assert!(compare_sizes(&size_check, out, 100, 110).is_none());
    assert!(compare_sizes(&size_check, out, 100, 50).is_none());
    assert!(compare_sizes(&size_check, out, 0, 100).is_none());
    let warning = compare_sizes(&size_check, SizeMetric::Closure, 100, 111);
    assert_eq!(warning.map(|w| w.metric), Some(SizeMetric::Closure));
}

#[test]
fn worse_warning_has_larger_increase() {
    let size_check = SizeCheck {
        max_increase_percent: 10.0,
        base_branch: "main".to_string(),
    };
    let output = compare_sizes(&size_check, SizeMetric::Output, 100, 120);
    let closure = compare_sizes(&size_check, SizeMetric::Closure, 100, 150);
    let worst = SizeWarning::worse(output.clone(), closure.clone());
    assert_eq!(worst.map(|w| w.metric), Some(SizeMetric::Closure));
    assert_eq!(SizeWarning::worse(output.clone(), None), output);
    assert_eq!(SizeWarning::worse(None, closure.clone()), closure);
}
