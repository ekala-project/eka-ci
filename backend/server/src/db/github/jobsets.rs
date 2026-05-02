// GitHub Jobset operations

use anyhow::Result;
use sqlx::{Pool, Sqlite};

use super::super::model::DrvId;
use super::types::{BaseJob, JobInfo, JobSetInfo};
use crate::github::JobDifference;
use crate::nix::nix_eval_jobs::NixEvalDrv;

pub async fn has_jobset(
    sha: &str,
    name: &str,
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<bool> {
    let result: Option<i64> = sqlx::query_scalar(
        "SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ? AND owner = ? AND repo_name = ?",
    )
    .bind(sha)
    .bind(name)
    .bind(owner)
    .bind(repo_name)
    .fetch_optional(pool)
    .await?;
    Ok(result.is_some())
}

pub async fn create_jobset(
    sha: &str,
    name: &str,
    owner: &str,
    repo_name: &str,
    config_json: Option<&str>,
    pool: &Pool<Sqlite>,
) -> Result<i64> {
    // Since the insert statement could be repetitive, we must separate inseration and rowid
    // selection
    sqlx::query(
        "INSERT INTO GitHubJobSets (sha, job, owner, repo_name, config_json) VALUES (?, ?, ?, ?, \
         ?)",
    )
    .bind(sha)
    .bind(name)
    .bind(owner)
    .bind(repo_name)
    .bind(config_json)
    .execute(pool)
    .await?;

    let result = sqlx::query_scalar(
        "SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ? AND owner = ? AND repo_name = ?",
    )
    .bind(sha)
    .bind(name)
    .bind(owner)
    .bind(repo_name)
    .fetch_one(pool)
    .await?;
    Ok(result)
}

/// Query jobs from a specific commit's jobset
/// Returns a mapping of job name -> drv_path for efficient lookup
pub async fn get_jobset_jobs_by_sha(
    sha: &str,
    job_name: &str,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<BaseJob>> {
    let jobs = sqlx::query_as::<_, (String, String)>(
        r#"
        SELECT j.name, d.drv_path
        FROM Job j
        INNER JOIN GitHubJobSets js ON j.jobset = js.ROWID
        INNER JOIN Drv d ON j.drv_id = d.ROWID
        WHERE js.sha = ? AND js.job = ?
        "#,
    )
    .bind(sha)
    .bind(job_name)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(name, drv_path)| BaseJob { name, drv_path })
    .collect();

    Ok(jobs)
}

pub async fn create_jobs_for_jobset(
    jobset_id: i64,
    jobs: &[NixEvalDrv],
    base_jobs: Option<&[BaseJob]>,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<()> {
    use std::str::FromStr;

    use crate::db::model::DrvId;

    if jobs.is_empty() {
        return Ok(());
    }

    // Build a lookup map from base jobs for O(1) difference computation
    let base_map: std::collections::HashMap<&str, &str> = base_jobs
        .unwrap_or(&[])
        .iter()
        .map(|bj| (bj.name.as_str(), bj.drv_path.as_str()))
        .collect();

    // Using a transaction should allow for the pool to batch statements
    // better than individual insertions + pool flush
    let mut tx = pool.begin().await?;

    // Convert all drv_paths to DrvIds first, collecting any errors
    let job_data: Vec<(DrvId, &str, i64)> = jobs
        .iter()
        .map(|job| {
            let drv_id = DrvId::from_str(&job.drv_path)?;

            // Compute difference: New (0), Changed (1), or Unchanged (defaults to New if no base)
            let difference = match base_map.get(job.attr.as_str()) {
                Some(base_drv_path) if *base_drv_path == job.drv_path => 0, /* Unchanged (same */
                // drv) - mark as
                // New
                Some(_) => 1, // Changed (different drv for same attr)
                None => 0,    // New (attr doesn't exist in base)
            };

            Ok((drv_id, job.attr.as_str(), difference))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Use QueryBuilder for batch insert with subqueries
    let mut query_builder =
        sqlx::QueryBuilder::new("INSERT INTO Job (jobset, drv_id, name, difference) VALUES ");

    for (i, (drv_id, attr, difference)) in job_data.iter().enumerate() {
        if i > 0 {
            query_builder.push(", ");
        }
        query_builder.push("(");
        query_builder.push_bind(jobset_id);
        query_builder.push(", (SELECT rowid FROM Drv WHERE drv_path = ");
        query_builder.push_bind(drv_id);
        query_builder.push(" LIMIT 1), ");
        query_builder.push_bind(attr);
        query_builder.push(", ");
        query_builder.push_bind(difference);
        query_builder.push(")");
    }

    query_builder.build().execute(&mut *tx).await?;

    tx.commit().await?;

    Ok(())
}

/// Get job information for a specific drv
/// Returns all jobsets that contain this drv, along with the job name and difference type
pub async fn get_job_info_for_drv(
    drv_id: &DrvId,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<JobInfo>> {
    let jobs = sqlx::query_as(
        r#"
        SELECT j.jobset as jobset_id, j.name, j.difference
        FROM Job j
        WHERE j.drv_id = (SELECT ROWID FROM Drv WHERE drv_path = ?)
        "#,
    )
    .bind(drv_id)
    .fetch_all(pool)
    .await?;

    Ok(jobs)
}

/// Get job configuration for a specific drv
/// Returns the config_json from the first jobset that contains this drv
pub async fn get_job_config_for_drv(
    drv_id: &DrvId,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Option<String>> {
    let config_json: Option<String> = sqlx::query_scalar(
        r#"
        SELECT g.config_json
        FROM Job j
        INNER JOIN GitHubJobSets g ON j.jobset = g.ROWID
        WHERE j.drv_id = (SELECT ROWID FROM Drv WHERE drv_path = ?)
        LIMIT 1
        "#,
    )
    .bind(drv_id)
    .fetch_optional(pool)
    .await?
    .flatten();

    Ok(config_json)
}

/// Check if all jobs in a jobset have reached a terminal state
/// Terminal states are: Completed (success or failure), TransitiveFailure, and Interrupted states
pub async fn all_jobs_concluded(jobset_id: i64, pool: &Pool<Sqlite>) -> anyhow::Result<bool> {
    // Query for jobs that are NOT in terminal states
    // Non-terminal states: Queued (0), Buildable (1), FailedRetry (2), Building (7), Blocked (100)
    let non_terminal_count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM Job j
        JOIN Drv d ON j.drv_id = d.ROWID
        WHERE j.jobset = ? AND d.build_state IN (0, 1, 2, 7, 100)
        "#,
    )
    .bind(jobset_id)
    .fetch_one(pool)
    .await?;

    Ok(non_terminal_count == 0)
}

/// Determine if any new or changed jobs in a jobset have failed
/// Returns true if there are failures in new or changed jobs
pub async fn jobset_has_new_or_changed_failures(
    jobset_id: i64,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<bool> {
    // Query for jobs where difference is New or Changed and build_state indicates failure
    // Failure states: Completed(Failure) (-1), TransitiveFailure (-2), and various Interrupted
    // states (negative)
    let failure_count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM Job j
        JOIN Drv d ON j.drv_id = d.ROWID
        WHERE j.jobset = ?
          AND j.difference IN (?, ?)
          AND d.build_state < 0
        "#,
    )
    .bind(jobset_id)
    .bind(JobDifference::New)
    .bind(JobDifference::Changed)
    .fetch_one(pool)
    .await?;

    Ok(failure_count > 0)
}

/// Get the jobset name and commit for a jobset ID
pub async fn get_jobset_by_id(
    jobset_id: i64,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Option<JobSetInfo>> {
    let jobset = sqlx::query_as(
        r#"
        SELECT sha, job, owner, repo_name
        FROM GitHubJobSets
        WHERE ROWID = ?
        "#,
    )
    .bind(jobset_id)
    .fetch_optional(pool)
    .await?;

    Ok(jobset)
}

pub async fn get_jobset_info(jobset_id: i64, pool: &Pool<Sqlite>) -> anyhow::Result<JobSetInfo> {
    let info =
        sqlx::query_as("SELECT sha, job, owner, repo_name FROM GitHubJobSets WHERE ROWID = ?")
            .bind(jobset_id)
            .fetch_one(pool)
            .await?;

    Ok(info)
}
