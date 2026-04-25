use anyhow::Result;
use octocrab::Octocrab;
use octocrab::models::checks::CheckRun as GHCheckRun;
use serde::Serialize;
use sqlx::{FromRow, Pool, Row, Sqlite};

use super::model::build_event::DrvBuildState;
use super::model::{Drv, DrvId};
use crate::github::JobDifference;
use crate::nix::nix_eval_jobs::NixEvalDrv;

#[derive(Clone, Debug, PartialEq, Eq, FromRow, Serialize)]
pub struct CheckRun {
    pub check_run_id: i64,
    pub repo_name: String,
    pub repo_owner: String,
    pub build_state: DrvBuildState,
    pub drv_path: DrvId,
}

impl CheckRun {
    pub async fn send_gh_update(
        &self,
        octocrab: &Octocrab,
        status: &DrvBuildState,
    ) -> Result<GHCheckRun> {
        let (gh_status, gh_conclusion) = status.as_gh_checkrun_state();

        let check_builder = octocrab.checks(&self.repo_owner, &self.repo_name);
        let mut check_update = check_builder
            .update_check_run(octocrab::models::CheckRunId(self.check_run_id as u64))
            .status(gh_status);

        if let Some(conclusion) = gh_conclusion {
            check_update = check_update.conclusion(conclusion);
        }

        let check_run = check_update.send().await?;
        Ok(check_run)
    }
}

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

/// Insert jobs where they reference the job and the drv
pub async fn create_jobs_for_jobset(
    jobset_id: i64,
    jobs: &[NixEvalDrv],
    pool: &Pool<Sqlite>,
) -> anyhow::Result<()> {
    use std::str::FromStr;

    use crate::db::model::DrvId;

    if jobs.is_empty() {
        return Ok(());
    }

    // Using a transaction should allow for the pool to batch statements
    // better than individual insertions + pool flush
    let mut tx = pool.begin().await?;

    // Convert all drv_paths to DrvIds first, collecting any errors
    let job_data: Vec<(DrvId, &str)> = jobs
        .iter()
        .map(|job| {
            let drv_id = DrvId::from_str(&job.drv_path)?;
            Ok((drv_id, job.attr.as_str()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    // Use QueryBuilder for batch insert with subqueries
    let mut query_builder =
        sqlx::QueryBuilder::new("INSERT INTO Job (jobset, drv_id, name) VALUES ");

    for (i, (drv_id, attr)) in job_data.iter().enumerate() {
        if i > 0 {
            query_builder.push(", ");
        }
        query_builder.push("(");
        query_builder.push_bind(jobset_id);
        query_builder.push(", (SELECT rowid FROM Drv WHERE drv_path = ");
        query_builder.push_bind(drv_id);
        query_builder.push(" LIMIT 1), ");
        query_builder.push_bind(attr);
        query_builder.push(")");
    }

    query_builder.build().execute(&mut *tx).await?;

    tx.commit().await?;

    Ok(())
}

/// Insert a new CheckRunInfo record
pub async fn insert_check_run_info(
    check_run_id: i64,
    drv_path: &DrvId,
    repo_name: &str,
    repo_owner: &str,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO CheckRunInfo (check_run_id, drv_id, repo_name, repo_owner)
        VALUES (?, (SELECT ROWID FROM Drv WHERE drv_path = ? LIMIT 1), ?, ?)
        "#,
    )
    .bind(check_run_id)
    .bind(drv_path)
    .bind(repo_name)
    .bind(repo_owner)
    .execute(pool)
    .await?;

    Ok(())
}

/// Return all checkruns which match a drv_path
pub async fn check_runs_for_drv_path(
    drv_path: &DrvId,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<CheckRun>> {
    let check_runs = sqlx::query_as(
        r#"
        SELECT check_run_id, repo_name, repo_owner, build_state, drv_path
        FROM CheckRun
        WHERE drv_path = ?
        "#,
    )
    .bind(drv_path)
    .fetch_all(pool)
    .await?;

    Ok(check_runs)
}

/// Return all checkruns for a specific commit SHA that are still active (queued, buildable, or
/// building)
pub async fn check_runs_for_commit(
    sha: &str,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<CheckRun>> {
    let check_runs = sqlx::query_as(
        r#"
        SELECT DISTINCT c.check_run_id, c.repo_name, c.repo_owner, d.build_state, d.drv_path
        FROM CheckRunInfo c
        INNER JOIN Drv d ON c.drv_id = d.ROWID
        INNER JOIN Job j ON j.drv_id = d.ROWID
        INNER JOIN GitHubJobSets g ON j.jobset = g.ROWID
        WHERE g.sha = ? AND d.build_state IN (0, 1, 7)
        "#,
    )
    .bind(sha)
    .fetch_all(pool)
    .await?;

    Ok(check_runs)
}

/// Select drvs which are present for a specific job
pub async fn jobs_for_jobset_id(job_id: i64, pool: &Pool<Sqlite>) -> anyhow::Result<Vec<Drv>> {
    let drvs = sqlx::query_as(
        r#"
        SELECT d.drv_path, d.system, d.required_system_features, d.is_fod, d.build_state, d.output_size, d.closure_size, d.pname, d.version, d.license_json, d.maintainers_json, d.meta_position, d.broken, d.insecure
        FROM Drv d
        INNER JOIN Job j ON d.ROWID = j.drv_id
        WHERE j.jobset = ?
        "#,
    )
    .bind(job_id)
    .fetch_all(pool)
    .await?;

    Ok(drvs)
}

/// Select drvs which are only present in the head_sha
pub async fn new_jobs(
    head_jobset_id: i64,
    base_jobset_id: i64,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<Drv>> {
    // Query for drvs only present in the head jobset
    let new_drvs: Vec<Drv> = sqlx::query_as(
        r#"
        SELECT d.drv_path, d.system, d.required_system_features, d.is_fod, d.build_state, d.output_size, d.closure_size, d.pname, d.version, d.license_json, d.maintainers_json, d.meta_position, d.broken, d.insecure
        FROM Drv d
        INNER JOIN
        (SELECT drv_id
          FROM Job
          WHERE jobset = ?
          EXCEPT
          SELECT a.drv_id
          FROM Job AS a
          INNER JOIN Job AS b
          ON a.name = b.name
          WHERE a.jobset = ? AND b.jobset = ?
        ) j ON d.ROWID = j.drv_id
        "#,
    )
    .bind(head_jobset_id)
    .bind(head_jobset_id)
    .bind(base_jobset_id)
    .fetch_all(pool)
    .await?;

    Ok(new_drvs)
}

/// Select drvs which are only present in the head_sha
pub async fn removed_jobs(
    head_jobset_id: i64,
    base_jobset_id: i64,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<String>> {
    // Query for drvs only present in the head jobset
    let removed_drvs: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT name
        FROM Job
        WHERE jobset = ?
        EXCEPT
        SELECT a.name
        FROM Job AS a
        INNER JOIN Job AS b
        ON a.name = b.name
        WHERE a.jobset = ?
        "#,
    )
    .bind(head_jobset_id)
    .bind(base_jobset_id)
    .fetch_all(pool)
    .await?;

    Ok(removed_drvs)
}

/// Select drvs which are only present in the head_sha
pub async fn job_difference(
    head_sha: &str,
    base_sha: &str,
    job_name: &str,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<(Vec<Drv>, Vec<Drv>, Vec<String>)> {
    use anyhow::Context;

    // First, get the jobset IDs for both head and base
    let head_jobset_id =
        sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
            .bind(head_sha)
            .bind(job_name)
            .fetch_optional(pool)
            .await?
            .context("Failed to find jobset for head sha")?;

    let maybe_base_jobset_id =
        sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
            .bind(base_sha)
            .bind(job_name)
            .fetch_optional(pool)
            .await?;

    // If there's no base jobset, treat all jobs as new values
    if maybe_base_jobset_id.is_none() {
        let new_jobs = jobs_for_jobset_id(head_jobset_id, pool).await?;
        return Ok((new_jobs, Vec::new(), Vec::new()));
    }
    let base_jobset_id: i64 = maybe_base_jobset_id.unwrap();

    // Query for drvs which differ in drv_id but share the same job name
    let changed_drvs = sqlx::query_as(
        r#"
        SELECT d.drv_path, d.system, d.required_system_features, d.is_fod, d.build_state, d.output_size, d.closure_size, d.pname, d.version, d.license_json, d.maintainers_json, d.meta_position, d.broken, d.insecure
        FROM Drv d
        INNER JOIN
        (
          SELECT a.drv_id
          FROM Job AS a
          INNER JOIN Job AS b
          ON a.name = b.name
          WHERE a.jobset = ? AND b.jobset = ? AND a.drv_id <> b.drv_id
        ) j ON d.ROWID = j.drv_id
        "#,
    )
    .bind(head_jobset_id)
    .bind(base_jobset_id)
    .fetch_all(pool)
    .await?;

    let new_drvs = new_jobs(head_jobset_id, base_jobset_id, pool).await?;

    // Removed jobs are just "new" when you invert direction, however, we just need Job name
    let removed_jobs = removed_jobs(base_jobset_id, head_jobset_id, pool).await?;

    Ok((new_drvs, changed_drvs, removed_jobs))
}

/// Update job difference types for a specific jobset
/// This should be called after computing job_difference to mark which jobs are New/Changed/Removed
pub async fn update_job_differences(
    jobset_id: i64,
    new_drv_ids: &[DrvId],
    changed_drv_ids: &[DrvId],
    pool: &Pool<Sqlite>,
) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;

    // Mark new jobs (difference = New, which is already the default, so we could skip this)
    for drv_id in new_drv_ids {
        sqlx::query(
            "UPDATE Job SET difference = ? WHERE jobset = ? AND drv_id = (SELECT ROWID FROM Drv \
             WHERE drv_path = ?)",
        )
        .bind(JobDifference::New)
        .bind(jobset_id)
        .bind(drv_id)
        .execute(&mut *tx)
        .await?;
    }

    // Mark changed jobs (difference = Changed)
    for drv_id in changed_drv_ids {
        sqlx::query(
            "UPDATE Job SET difference = ? WHERE jobset = ? AND drv_id = (SELECT ROWID FROM Drv \
             WHERE drv_path = ?)",
        )
        .bind(JobDifference::Changed)
        .bind(jobset_id)
        .bind(drv_id)
        .execute(&mut *tx)
        .await?;
    }

    // Note: Removed jobs don't exist in the head commit's jobset, so we don't update them here
    // They would only exist in the base commit's jobset

    tx.commit().await?;
    Ok(())
}

#[derive(Debug, FromRow)]
pub struct JobInfo {
    pub jobset_id: i64,
    pub name: String,
    pub difference: JobDifference,
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
#[derive(Debug, FromRow)]
pub struct JobSetInfo {
    pub sha: String,
    pub job: String,
    pub owner: String,
    pub repo_name: String,
}

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

/// Repository information returned by the API
#[derive(Debug, FromRow, Serialize)]
pub struct RepositoryInfo {
    pub owner: String,
    pub repo_name: String,
    pub installation_id: i64,
}

/// Get all repositories being tracked
/// This now uses the GitHubInstallationRepositories table to show ALL installed repos,
/// even if they haven't had any builds yet
pub async fn list_repositories(pool: &Pool<Sqlite>) -> Result<Vec<RepositoryInfo>> {
    let repos = sqlx::query_as(
        r#"
        SELECT
            repo_owner as owner,
            repo_name,
            installation_id
        FROM GitHubInstallationRepositories
        ORDER BY repo_owner, repo_name
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(repos)
}

/// Get information about a specific repository
pub async fn get_repository(
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<RepositoryInfo>> {
    let repo = sqlx::query_as(
        r#"
        SELECT
            repo_owner as owner,
            repo_name,
            installation_id
        FROM GitHubInstallationRepositories
        WHERE repo_owner = ? AND repo_name = ?
        LIMIT 1
        "#,
    )
    .bind(owner)
    .bind(repo_name)
    .fetch_optional(pool)
    .await?;

    Ok(repo)
}

/// Commit information with build status
#[derive(Debug, FromRow, Serialize)]
pub struct CommitInfo {
    pub sha: String,
    pub job_count: i64,
}

/// Get recent commits for a repository
pub async fn list_repository_commits(
    owner: &str,
    repo_name: &str,
    limit: i64,
    pool: &Pool<Sqlite>,
) -> Result<Vec<CommitInfo>> {
    let commits = sqlx::query_as(
        r#"
        SELECT
            sha,
            COUNT(DISTINCT job) as job_count
        FROM GitHubJobSets
        WHERE owner = ? AND repo_name = ?
        GROUP BY sha
        ORDER BY ROWID DESC
        LIMIT ?
        "#,
    )
    .bind(owner)
    .bind(repo_name)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    Ok(commits)
}

/// Job details for a specific jobset
#[derive(Debug, FromRow, Serialize)]
pub struct JobSetDetails {
    pub jobset_id: i64,
    pub job_name: String,
    pub sha: String,
    pub owner: String,
    pub repo_name: String,
    pub total_drvs: i64,
    pub queued_drvs: i64,
    pub buildable_drvs: i64,
    pub building_drvs: i64,
    pub completed_success_drvs: i64,
    pub completed_failure_drvs: i64,
    pub failed_retry_drvs: i64,
    pub transitive_failure_drvs: i64,
    pub blocked_drvs: i64,
    pub interrupted_drvs: i64,
}

/// Get detailed information about a jobset including build statistics
pub async fn get_jobset_details(jobset_id: i64, pool: &Pool<Sqlite>) -> Result<JobSetDetails> {
    let details = sqlx::query_as(
        r#"
        SELECT
            g.ROWID as jobset_id,
            g.job as job_name,
            g.sha,
            g.owner,
            g.repo_name,
            COUNT(d.ROWID) as total_drvs,
            SUM(CASE WHEN d.build_state = 0 THEN 1 ELSE 0 END) as queued_drvs,
            SUM(CASE WHEN d.build_state = 1 THEN 1 ELSE 0 END) as buildable_drvs,
            SUM(CASE WHEN d.build_state = 7 THEN 1 ELSE 0 END) as building_drvs,
            SUM(CASE WHEN d.build_state = 100 THEN 1 ELSE 0 END) as blocked_drvs,
            SUM(CASE WHEN d.build_state = 1000 THEN 1 ELSE 0 END) as completed_success_drvs,
            SUM(CASE WHEN d.build_state = -1 THEN 1 ELSE 0 END) as completed_failure_drvs,
            SUM(CASE WHEN d.build_state = 2 THEN 1 ELSE 0 END) as failed_retry_drvs,
            SUM(CASE WHEN d.build_state = -2 THEN 1 ELSE 0 END) as transitive_failure_drvs,
            SUM(CASE WHEN d.build_state < -2 THEN 1 ELSE 0 END) as interrupted_drvs
        FROM GitHubJobSets g
        JOIN Job j ON j.jobset = g.ROWID
        JOIN Drv d ON d.ROWID = j.drv_id
        WHERE g.ROWID = ?
        GROUP BY g.ROWID
        "#,
    )
    .bind(jobset_id)
    .fetch_one(pool)
    .await?;

    Ok(details)
}

/// Get all drvs for a specific jobset
#[derive(Debug, FromRow, Serialize)]
pub struct JobSetDrv {
    pub drv_path: DrvId,
    pub name: String,
    pub system: String,
    pub build_state: DrvBuildState,
    pub is_fod: bool,
    pub difference: JobDifference,
}

pub async fn get_jobset_drvs(
    jobset_id: i64,
    state_filter: Option<DrvBuildState>,
    limit: i64,
    offset: i64,
    pool: &Pool<Sqlite>,
) -> Result<Vec<JobSetDrv>> {
    let drvs = if let Some(state) = state_filter {
        sqlx::query_as(
            r#"
            SELECT
                d.drv_path,
                j.name,
                d.system,
                d.build_state,
                d.is_fod,
                j.difference
            FROM Job j
            JOIN Drv d ON d.ROWID = j.drv_id
            WHERE j.jobset = ? AND d.build_state = ?
            ORDER BY j.name
            LIMIT ? OFFSET ?
            "#,
        )
        .bind(jobset_id)
        .bind(state)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as(
            r#"
            SELECT
                d.drv_path,
                j.name,
                d.system,
                d.build_state,
                d.is_fod,
                j.difference
            FROM Job j
            JOIN Drv d ON d.ROWID = j.drv_id
            WHERE j.jobset = ?
            ORDER BY j.name
            LIMIT ? OFFSET ?
            "#,
        )
        .bind(jobset_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await?
    };

    Ok(drvs)
}

/// Count total drvs in a jobset
pub async fn count_jobset_drvs(jobset_id: i64, pool: &Pool<Sqlite>) -> Result<i64> {
    let count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM Job j
        WHERE j.jobset = ?
        "#,
    )
    .bind(jobset_id)
    .fetch_one(pool)
    .await?;

    Ok(count)
}

/// Get all active jobs (jobs with any queued, buildable, or building drvs)
pub async fn get_active_jobs(pool: &Pool<Sqlite>) -> Result<Vec<JobSetDetails>> {
    let jobs = sqlx::query_as(
        r#"
        SELECT
            g.ROWID as jobset_id,
            g.job as job_name,
            g.sha,
            g.owner,
            g.repo_name,
            COUNT(d.ROWID) as total_drvs,
            SUM(CASE WHEN d.build_state = 0 THEN 1 ELSE 0 END) as queued_drvs,
            SUM(CASE WHEN d.build_state = 1 THEN 1 ELSE 0 END) as buildable_drvs,
            SUM(CASE WHEN d.build_state = 7 THEN 1 ELSE 0 END) as building_drvs,
            SUM(CASE WHEN d.build_state = 100 THEN 1 ELSE 0 END) as blocked_drvs,
            SUM(CASE WHEN d.build_state = 1000 THEN 1 ELSE 0 END) as completed_success_drvs,
            SUM(CASE WHEN d.build_state = -1 THEN 1 ELSE 0 END) as completed_failure_drvs,
            SUM(CASE WHEN d.build_state = 2 THEN 1 ELSE 0 END) as failed_retry_drvs,
            SUM(CASE WHEN d.build_state = -2 THEN 1 ELSE 0 END) as transitive_failure_drvs,
            SUM(CASE WHEN d.build_state < -2 THEN 1 ELSE 0 END) as interrupted_drvs
        FROM GitHubJobSets g
        JOIN Job j ON j.jobset = g.ROWID
        JOIN Drv d ON d.ROWID = j.drv_id
        GROUP BY g.ROWID
        HAVING SUM(CASE WHEN d.build_state IN (0, 1, 7) THEN 1 ELSE 0 END) > 0
        ORDER BY g.ROWID DESC
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(jobs)
}

/// A building derivation that may or may not be associated with a job
#[derive(Debug, FromRow, Serialize)]
pub struct BuildingDrv {
    pub drv_path: DrvId,
    pub name: Option<String>, // Name from Job table if associated
    pub system: String,
    pub build_state: DrvBuildState,
    pub is_fod: bool,
    pub difference: Option<JobDifference>, // Difference from Job table if associated
}

/// Get all building drvs (including intermediate dependencies not directly in jobs)
pub async fn get_all_building_drvs(pool: &Pool<Sqlite>) -> Result<Vec<BuildingDrv>> {
    let drvs = sqlx::query_as(
        r#"
        SELECT
            d.drv_path,
            j.name,
            d.system,
            d.build_state,
            d.is_fod,
            j.difference
        FROM Drv d
        LEFT JOIN Job j ON d.ROWID = j.drv_id
        WHERE d.build_state = 7
        ORDER BY COALESCE(j.name, d.drv_path)
        "#,
    )
    .fetch_all(pool)
    .await?;

    Ok(drvs)
}

/// Get all jobs for a commit
#[derive(Debug, FromRow, Serialize)]
pub struct CommitJob {
    pub jobset_id: i64,
    pub job_name: String,
    pub total_drvs: i64,
    pub completed_drvs: i64,
    pub failed_drvs: i64,
}

pub async fn get_commit_jobs(sha: &str, pool: &Pool<Sqlite>) -> Result<Vec<CommitJob>> {
    let jobs = sqlx::query_as(
        r#"
        SELECT
            g.ROWID as jobset_id,
            g.job as job_name,
            COUNT(d.ROWID) as total_drvs,
            SUM(CASE WHEN d.build_state >= 1000 OR d.build_state < 0 THEN 1 ELSE 0 END) as completed_drvs,
            SUM(CASE WHEN d.build_state < 0 THEN 1 ELSE 0 END) as failed_drvs
        FROM GitHubJobSets g
        JOIN Job j ON j.jobset = g.ROWID
        JOIN Drv d ON d.ROWID = j.drv_id
        WHERE g.sha = ?
        GROUP BY g.ROWID
        "#,
    )
    .bind(sha)
    .fetch_all(pool)
    .await?;

    Ok(jobs)
}

// ============================================================================
// Pull Request Management
// ============================================================================

/// Information about a pull request
#[derive(Clone, Debug, PartialEq, Eq, FromRow, Serialize)]
pub struct PullRequestInfo {
    pub pr_number: i64,
    pub owner: String,
    pub repo_name: String,
    pub head_sha: String,
    pub base_sha: String,
    pub title: String,
    pub author: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
}

/// Pull request with associated job statistics
#[derive(Clone, Debug, Serialize)]
pub struct PullRequestWithStats {
    #[serde(flatten)]
    pub pr_info: PullRequestInfo,
    pub jobset_id: Option<i64>,
    pub total_drvs: i64,
    pub completed_success_drvs: i64,
    pub completed_failure_drvs: i64,
    pub failed_retry_drvs: i64,
    pub changed_drvs: i64,
    pub new_drvs: i64,
}

/// Upsert a pull request record (insert or update if exists)
pub async fn upsert_pull_request(
    pr_number: i64,
    owner: &str,
    repo_name: &str,
    head_sha: &str,
    base_sha: &str,
    title: &str,
    author: &str,
    state: &str,
    created_at: &str,
    updated_at: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO PullRequests
            (pr_number, owner, repo_name, head_sha, base_sha, title, author, state, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(owner, repo_name, pr_number) DO UPDATE SET
            head_sha = excluded.head_sha,
            base_sha = excluded.base_sha,
            title = excluded.title,
            state = excluded.state,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(pr_number)
    .bind(owner)
    .bind(repo_name)
    .bind(head_sha)
    .bind(base_sha)
    .bind(title)
    .bind(author)
    .bind(state)
    .bind(created_at)
    .bind(updated_at)
    .execute(pool)
    .await?;

    Ok(())
}

/// Upsert a merge queue build record
/// Uses pr_number=0 as a sentinel value for merge queue builds
pub async fn upsert_merge_queue_build(
    owner: &str,
    repo_name: &str,
    head_sha: &str,
    base_sha: &str,
    base_ref: &str,
    merge_group_head_sha: &str,
    created_at: &str,
    updated_at: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let title = format!("Merge queue for {}", base_ref);

    sqlx::query(
        r#"
        INSERT INTO PullRequests
            (pr_number, owner, repo_name, head_sha, base_sha, title, author, state, created_at, updated_at, is_merge_queue, merge_group_head_sha)
        VALUES (0, ?, ?, ?, ?, ?, 'github-merge-queue', 'open', ?, ?, TRUE, ?)
        ON CONFLICT(owner, repo_name, pr_number) DO UPDATE SET
            head_sha = excluded.head_sha,
            base_sha = excluded.base_sha,
            title = excluded.title,
            updated_at = excluded.updated_at,
            merge_group_head_sha = excluded.merge_group_head_sha
        "#,
    )
    .bind(owner)
    .bind(repo_name)
    .bind(head_sha)
    .bind(base_sha)
    .bind(&title)
    .bind(created_at)
    .bind(updated_at)
    .bind(merge_group_head_sha)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get all open pull requests with their build statistics
pub async fn list_open_pull_requests(pool: &Pool<Sqlite>) -> Result<Vec<PullRequestWithStats>> {
    // Query for PRs and their basic info
    let pr_rows = sqlx::query(
        r#"
        SELECT
            pr.pr_number,
            pr.owner,
            pr.repo_name,
            pr.head_sha,
            pr.base_sha,
            pr.title,
            pr.author,
            pr.state,
            pr.created_at,
            pr.updated_at,
            g.ROWID as jobset_id,
            COALESCE(COUNT(d.ROWID), 0) as total_drvs,
            COALESCE(SUM(CASE WHEN d.build_state >= 6 AND d.build_state < 11 THEN 1 ELSE 0 END), 0) as completed_success_drvs,
            COALESCE(SUM(CASE WHEN d.build_state >= 11 AND d.build_state < 16 THEN 1 ELSE 0 END), 0) as completed_failure_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 2 THEN 1 ELSE 0 END), 0) as failed_retry_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 1 THEN 1 ELSE 0 END), 0) as changed_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 0 THEN 1 ELSE 0 END), 0) as new_drvs
        FROM PullRequests pr
        LEFT JOIN GitHubJobSets g ON pr.head_sha = g.sha
        LEFT JOIN Job j ON g.ROWID = j.jobset
        LEFT JOIN Drv d ON j.drv_id = d.ROWID
        WHERE pr.state = 'open'
        GROUP BY pr.pr_number, pr.owner, pr.repo_name
        ORDER BY pr.updated_at DESC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let prs = pr_rows
        .into_iter()
        .map(|row| PullRequestWithStats {
            pr_info: PullRequestInfo {
                pr_number: row.get("pr_number"),
                owner: row.get("owner"),
                repo_name: row.get("repo_name"),
                head_sha: row.get("head_sha"),
                base_sha: row.get("base_sha"),
                title: row.get("title"),
                author: row.get("author"),
                state: row.get("state"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            },
            jobset_id: row.get("jobset_id"),
            total_drvs: row.get("total_drvs"),
            completed_success_drvs: row.get("completed_success_drvs"),
            completed_failure_drvs: row.get("completed_failure_drvs"),
            failed_retry_drvs: row.get("failed_retry_drvs"),
            changed_drvs: row.get("changed_drvs"),
            new_drvs: row.get("new_drvs"),
        })
        .collect();

    Ok(prs)
}

/// Get a specific pull request with build statistics
pub async fn get_pull_request(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<PullRequestWithStats>> {
    let pr_row = sqlx::query(
        r#"
        SELECT
            pr.pr_number,
            pr.owner,
            pr.repo_name,
            pr.head_sha,
            pr.base_sha,
            pr.title,
            pr.author,
            pr.state,
            pr.created_at,
            pr.updated_at,
            g.ROWID as jobset_id,
            COALESCE(COUNT(d.ROWID), 0) as total_drvs,
            COALESCE(SUM(CASE WHEN d.build_state >= 6 AND d.build_state < 11 THEN 1 ELSE 0 END), 0) as completed_success_drvs,
            COALESCE(SUM(CASE WHEN d.build_state >= 11 AND d.build_state < 16 THEN 1 ELSE 0 END), 0) as completed_failure_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 2 THEN 1 ELSE 0 END), 0) as failed_retry_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 1 THEN 1 ELSE 0 END), 0) as changed_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 0 THEN 1 ELSE 0 END), 0) as new_drvs
        FROM PullRequests pr
        LEFT JOIN GitHubJobSets g ON pr.head_sha = g.sha
        LEFT JOIN Job j ON g.ROWID = j.jobset
        LEFT JOIN Drv d ON j.drv_id = d.ROWID
        WHERE pr.owner = ? AND pr.repo_name = ? AND pr.pr_number = ?
        GROUP BY pr.pr_number, pr.owner, pr.repo_name
        "#,
    )
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .fetch_optional(pool)
    .await?;

    Ok(pr_row.map(|row| PullRequestWithStats {
        pr_info: PullRequestInfo {
            pr_number: row.get("pr_number"),
            owner: row.get("owner"),
            repo_name: row.get("repo_name"),
            head_sha: row.get("head_sha"),
            base_sha: row.get("base_sha"),
            title: row.get("title"),
            author: row.get("author"),
            state: row.get("state"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        },
        jobset_id: row.get("jobset_id"),
        total_drvs: row.get("total_drvs"),
        completed_success_drvs: row.get("completed_success_drvs"),
        completed_failure_drvs: row.get("completed_failure_drvs"),
        failed_retry_drvs: row.get("failed_retry_drvs"),
        changed_drvs: row.get("changed_drvs"),
        new_drvs: row.get("new_drvs"),
    }))
}

/// Get all merge queue builds for a repository with their build statistics
pub async fn list_merge_queue_builds(
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<Vec<PullRequestWithStats>> {
    let pr_rows = sqlx::query(
        r#"
        SELECT
            pr.pr_number,
            pr.owner,
            pr.repo_name,
            pr.head_sha,
            pr.base_sha,
            pr.title,
            pr.author,
            pr.state,
            pr.created_at,
            pr.updated_at,
            g.ROWID as jobset_id,
            COALESCE(COUNT(d.ROWID), 0) as total_drvs,
            COALESCE(SUM(CASE WHEN d.build_state >= 6 AND d.build_state < 11 THEN 1 ELSE 0 END), 0) as completed_success_drvs,
            COALESCE(SUM(CASE WHEN d.build_state >= 11 AND d.build_state < 16 THEN 1 ELSE 0 END), 0) as completed_failure_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 2 THEN 1 ELSE 0 END), 0) as failed_retry_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 1 THEN 1 ELSE 0 END), 0) as changed_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 0 THEN 1 ELSE 0 END), 0) as new_drvs
        FROM PullRequests pr
        LEFT JOIN GitHubJobSets g ON pr.head_sha = g.sha
        LEFT JOIN Job j ON g.ROWID = j.jobset
        LEFT JOIN Drv d ON j.drv_id = d.ROWID
        WHERE pr.owner = ? AND pr.repo_name = ? AND pr.is_merge_queue = TRUE
        GROUP BY pr.pr_number, pr.owner, pr.repo_name
        ORDER BY pr.updated_at DESC
        "#,
    )
    .bind(owner)
    .bind(repo_name)
    .fetch_all(pool)
    .await?;

    let builds = pr_rows
        .into_iter()
        .map(|row| PullRequestWithStats {
            pr_info: PullRequestInfo {
                pr_number: row.get("pr_number"),
                owner: row.get("owner"),
                repo_name: row.get("repo_name"),
                head_sha: row.get("head_sha"),
                base_sha: row.get("base_sha"),
                title: row.get("title"),
                author: row.get("author"),
                state: row.get("state"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            },
            jobset_id: row.get("jobset_id"),
            total_drvs: row.get("total_drvs"),
            completed_success_drvs: row.get("completed_success_drvs"),
            completed_failure_drvs: row.get("completed_failure_drvs"),
            failed_retry_drvs: row.get("failed_retry_drvs"),
            changed_drvs: row.get("changed_drvs"),
            new_drvs: row.get("new_drvs"),
        })
        .collect();

    Ok(builds)
}

/// Get a specific merge queue build by commit SHA
pub async fn get_merge_queue_build_by_sha(
    owner: &str,
    repo_name: &str,
    sha: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<PullRequestWithStats>> {
    let pr_row = sqlx::query(
        r#"
        SELECT
            pr.pr_number,
            pr.owner,
            pr.repo_name,
            pr.head_sha,
            pr.base_sha,
            pr.title,
            pr.author,
            pr.state,
            pr.created_at,
            pr.updated_at,
            g.ROWID as jobset_id,
            COALESCE(COUNT(d.ROWID), 0) as total_drvs,
            COALESCE(SUM(CASE WHEN d.build_state >= 6 AND d.build_state < 11 THEN 1 ELSE 0 END), 0) as completed_success_drvs,
            COALESCE(SUM(CASE WHEN d.build_state >= 11 AND d.build_state < 16 THEN 1 ELSE 0 END), 0) as completed_failure_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 2 THEN 1 ELSE 0 END), 0) as failed_retry_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 1 THEN 1 ELSE 0 END), 0) as changed_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 0 THEN 1 ELSE 0 END), 0) as new_drvs
        FROM PullRequests pr
        LEFT JOIN GitHubJobSets g ON pr.head_sha = g.sha
        LEFT JOIN Job j ON g.ROWID = j.jobset
        LEFT JOIN Drv d ON j.drv_id = d.ROWID
        WHERE pr.owner = ? AND pr.repo_name = ? AND pr.is_merge_queue = TRUE AND pr.head_sha = ?
        GROUP BY pr.pr_number, pr.owner, pr.repo_name
        "#,
    )
    .bind(owner)
    .bind(repo_name)
    .bind(sha)
    .fetch_optional(pool)
    .await?;

    Ok(pr_row.map(|row| PullRequestWithStats {
        pr_info: PullRequestInfo {
            pr_number: row.get("pr_number"),
            owner: row.get("owner"),
            repo_name: row.get("repo_name"),
            head_sha: row.get("head_sha"),
            base_sha: row.get("base_sha"),
            title: row.get("title"),
            author: row.get("author"),
            state: row.get("state"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        },
        jobset_id: row.get("jobset_id"),
        total_drvs: row.get("total_drvs"),
        completed_success_drvs: row.get("completed_success_drvs"),
        completed_failure_drvs: row.get("completed_failure_drvs"),
        failed_retry_drvs: row.get("failed_retry_drvs"),
        changed_drvs: row.get("changed_drvs"),
        new_drvs: row.get("new_drvs"),
    }))
}

// ========================================
// Auto-Merge Functions
// ========================================

/// Enable auto-merge for a pull request
pub async fn enable_auto_merge(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    merge_method: Option<&str>,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        "UPDATE PullRequests
         SET auto_merge_enabled = TRUE, merge_method = ?
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(merge_method)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;
    Ok(())
}

/// Disable auto-merge for a pull request
pub async fn disable_auto_merge(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        "UPDATE PullRequests
         SET auto_merge_enabled = FALSE, merge_method = NULL
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;
    Ok(())
}

/// Mark a PR as merged; also clears any pending comment-merge.
pub async fn mark_pr_merged(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    merged_by_user_id: Option<i64>,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        "UPDATE PullRequests
         SET state = 'merged',
             merged_by_user_id = ?,
             merged_at = CURRENT_TIMESTAMP,
             comment_merge_sha = NULL,
             comment_merge_method = NULL,
             comment_merge_requester_id = NULL,
             comment_merge_requester_login = NULL,
             comment_merge_comment_id = NULL,
             comment_merge_requested_at = NULL
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(merged_by_user_id)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;
    Ok(())
}

/// Record a pending `@eka-ci merge` on a PR; overwrites any prior
/// request (single active request per PR). Returns rows updated
/// (0 means the PR row is missing — callers should treat as error).
pub async fn set_comment_merge_request(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    sha: &str,
    method: Option<&str>,
    requester_id: i64,
    requester_login: &str,
    comment_id: i64,
    pool: &Pool<Sqlite>,
) -> Result<u64> {
    let rows = sqlx::query(
        "UPDATE PullRequests
         SET comment_merge_sha = ?,
             comment_merge_method = ?,
             comment_merge_requester_id = ?,
             comment_merge_requester_login = ?,
             comment_merge_comment_id = ?,
             comment_merge_requested_at = CURRENT_TIMESTAMP
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(sha)
    .bind(method)
    .bind(requester_id)
    .bind(requester_login)
    .bind(comment_id)
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(rows)
}

/// Clear any pending comment-merge on a PR (on merge success, SHA
/// drift, explicit cancel, or PR close).
pub async fn clear_comment_merge_request(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        "UPDATE PullRequests
         SET comment_merge_sha = NULL,
             comment_merge_method = NULL,
             comment_merge_requester_id = NULL,
             comment_merge_requester_login = NULL,
             comment_merge_comment_id = NULL,
             comment_merge_requested_at = NULL
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .execute(pool)
    .await?;
    Ok(())
}

/// Fetch the pending comment-merge for a PR; `None` if none
/// outstanding or PR missing.
#[allow(dead_code)]
pub async fn get_comment_merge_request(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<CommentMergeRequest>> {
    let pr = sqlx::query_as::<_, PullRequest>(
        "SELECT * FROM PullRequests
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .fetch_optional(pool)
    .await?;
    Ok(pr.and_then(|pr| pr.pending_comment_merge()))
}

/// Get PR by head SHA (for finding PR from jobset)
pub async fn get_pr_by_head_sha(
    sha: &str,
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<PullRequest>> {
    let pr = sqlx::query_as::<_, PullRequest>(
        "SELECT * FROM PullRequests
         WHERE head_sha = ? AND owner = ? AND repo_name = ?",
    )
    .bind(sha)
    .bind(owner)
    .bind(repo_name)
    .fetch_optional(pool)
    .await?;
    Ok(pr)
}

/// Fetch the raw `PullRequest` row (no stats). For callers needing
/// `comment_merge_*` / `auto_merge_enabled` / `head_sha`; use
/// `get_pull_request` for the stats-augmented variant.
pub async fn get_pull_request_row(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<PullRequest>> {
    let pr = sqlx::query_as::<_, PullRequest>(
        "SELECT * FROM PullRequests
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .fetch_optional(pool)
    .await?;
    Ok(pr)
}

/// Get attribute paths (packages) changed in a PR by comparing base_sha to head_sha
pub async fn get_pr_changed_packages(
    pr_number: i64,
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<Vec<String>> {
    // Get the PR's head_sha jobset
    let jobset_id: Option<i64> = sqlx::query_scalar(
        "SELECT jobset_id FROM PullRequests pr
         JOIN GitHubJobSets gjs ON pr.head_sha = gjs.sha
         WHERE pr.pr_number = ? AND pr.owner = ? AND pr.repo_name = ?
         AND gjs.owner = ? AND gjs.repo_name = ?
         LIMIT 1",
    )
    .bind(pr_number)
    .bind(owner)
    .bind(repo_name)
    .bind(owner)
    .bind(repo_name)
    .fetch_optional(pool)
    .await?;

    let Some(jobset_id) = jobset_id else {
        // No jobset found, return empty list
        return Ok(vec![]);
    };

    // Get all unique attribute paths from job_difference for this jobset
    // Focus on changed and new packages (not removed)
    let attr_paths: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT attr_path FROM job_difference
         WHERE jobset_id = ? AND status IN ('changed', 'new')",
    )
    .bind(jobset_id)
    .fetch_all(pool)
    .await?;

    Ok(attr_paths)
}

/// Whether a PR's head commit has a fully built, non-failing jobset recorded.
///
/// Returns `true` only when:
/// - A jobset exists for the PR's head SHA in this owner/repo,
/// - every job in that jobset has reached a terminal state, and
/// - no new or changed jobs are in a failure state.
///
/// Intended as a guard before attempting approval-gated auto-merge, so that a
/// review submitted before builds complete does not cause premature merging.
pub async fn pr_head_build_succeeded(
    pr_number: i64,
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<bool> {
    let jobset_id: Option<i64> = sqlx::query_scalar(
        "SELECT gjs.ROWID FROM PullRequests pr
         JOIN GitHubJobSets gjs ON pr.head_sha = gjs.sha
         WHERE pr.pr_number = ? AND pr.owner = ? AND pr.repo_name = ?
         AND gjs.owner = ? AND gjs.repo_name = ?
         LIMIT 1",
    )
    .bind(pr_number)
    .bind(owner)
    .bind(repo_name)
    .bind(owner)
    .bind(repo_name)
    .fetch_optional(pool)
    .await?;

    let Some(jobset_id) = jobset_id else {
        return Ok(false);
    };

    if !all_jobs_concluded(jobset_id, pool).await? {
        return Ok(false);
    }

    if jobset_has_new_or_changed_failures(jobset_id, pool).await? {
        return Ok(false);
    }

    Ok(true)
}

// Database row struct — all fields mirror the `PullRequests` schema so
// SQLx `FromRow` can hydrate them, even when Rust callers only read a subset.
#[allow(dead_code)]
#[derive(Debug, Clone, FromRow)]
pub struct PullRequest {
    pub pr_number: i64,
    pub owner: String,
    pub repo_name: String,
    pub head_sha: String,
    pub base_sha: String,
    pub title: String,
    pub author: String,
    pub state: String,
    pub created_at: String,
    pub updated_at: String,
    pub jobset_id: Option<i64>,
    pub is_merge_queue: bool,
    pub merge_group_head_sha: Option<String>,
    pub auto_merge_enabled: bool,
    pub merge_method: Option<String>,
    pub merged_by_user_id: Option<i64>,
    pub merged_at: Option<String>,
    // Pending `@eka-ci merge` request; all six NULL iff none pending.
    // See 20260419_pr_comment_merge.sql.
    pub comment_merge_sha: Option<String>,
    pub comment_merge_method: Option<String>,
    pub comment_merge_requester_id: Option<i64>,
    pub comment_merge_requester_login: Option<String>,
    pub comment_merge_comment_id: Option<i64>,
    pub comment_merge_requested_at: Option<String>,
}

/// Typed view of a pending `@eka-ci merge` on a PR (unresolved: not yet
/// merged, cancelled, or superseded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentMergeRequest {
    pub sha: String,
    pub method: Option<String>,
    pub requester_id: i64,
    pub requester_login: String,
    pub comment_id: i64,
    pub requested_at: String,
}

impl PullRequest {
    /// Pending comment-merge, if any. `comment_merge_*` columns are
    /// set together, so `comment_merge_sha` being `Some` implies the rest.
    pub fn pending_comment_merge(&self) -> Option<CommentMergeRequest> {
        Some(CommentMergeRequest {
            sha: self.comment_merge_sha.clone()?,
            method: self.comment_merge_method.clone(),
            requester_id: self.comment_merge_requester_id?,
            requester_login: self.comment_merge_requester_login.clone()?,
            comment_id: self.comment_merge_comment_id?,
            requested_at: self.comment_merge_requested_at.clone()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use sqlx::SqlitePool;

    use super::*;
    use crate::db::model::Drv;
    use crate::db::model::build_event::DrvBuildState;
    use crate::db::model::drv::insert_drv;
    use crate::db::model::drv_id::DrvId;

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn create_github_jobs(pool: SqlitePool) -> anyhow::Result<()> {
        use std::str::FromStr;

        let eval_drv_str = r#"{"attr":"cmake","attrPath":["cmake"],"drvPath":"/nix/store/3fr8b3xlygv2a64ff7fq7564j4sxv4lc-cmake-3.29.6.drv","inputDrvs":{"/nix/store/08s4j5nvddsbrjpachqwzai83xngxnc0-pkg-config-wrapper-0.29.2.drv":["out"],"/nix/store/0cgbdlz63qiqf5f8i1sljak1dfbzyrl5-openssl-3.0.14.drv":["dev"],"/nix/store/265x0i426vnqjma9khcfpi86m6hx4smr-bash-5.2p32.drv":["out"],"/nix/store/27zlixdsk0kx585j4dcjm53636mx7cis-libuv-1.48.0.drv":["dev"],"/nix/store/2vyizsckka60lhh0kylhbpdd1flb998v-cmake-3.29.6.tar.gz.drv":["out"],"/nix/store/4hzjv6r5v7h6hzad718jgc0hrm1gz8r1-gcc-wrapper-13.3.0.drv":["out"],"/nix/store/860zddz386bk0441flrg940ipbp0jp1z-xz-5.6.2.drv":["dev"],"/nix/store/9jvlq6qg9j1222w3zm3wgfv5qyqfqmxz-bzip2-1.0.8.drv":["dev"],"/nix/store/ax4q30iyf9wi95hswil021lg0cdqq6rl-libarchive-3.7.4.drv":["dev"],"/nix/store/bxq3kjf71wn92yisdbq18fzpvcl5pn31-expat-2.6.2.drv":["dev"],"/nix/store/kh6mps96srqgdvn03vq4gmqzl51s9w8h-glibc-2.39-52.drv":["bin","dev","out"],"/nix/store/lzc503qcc7f6ibq8sdbcri73wb62dj4r-zlib-1.3.1.drv":["dev"],"/nix/store/mzw7jzs6ix17ajh3z4kqzvh8l7abj4yr-rhash-1.4.4.drv":["out"],"/nix/store/v288gxsg679gyi9zpg0mhrv26vfmw4kr-stdenv-linux.drv":["out"],"/nix/store/vnq47hr4nwry8kgvfgmx0229id3q49dr-binutils-2.42.drv":["out"],"/nix/store/y99v9h2mcqbw91g7p3lnk292k0np0djr-curl-8.9.0.drv":["dev"]},"name":"cmake-3.29.6","outputs":{"debug":"/nix/store/xrh9g28kmsyjlw6qf46ngkvhac1llgvz-cmake-3.29.6-debug","out":"/nix/store/rz7j0kdkq8j522vpw6n8wjq2qv3if24g-cmake-3.29.6"},"system":"x86_64-linux"}"#;

        let eval_drv =
            serde_json::from_str::<NixEvalDrv>(eval_drv_str).expect("Failed to deserialize output");
        let mut eval_drv2 = eval_drv.clone();
        eval_drv2.drv_path =
            "/nix/store/3fr8baalygv2a64ff7fq7564j4sxv4lc-cmake-3.29.6.drv".to_string();

        let drv = Drv {
            drv_path: DrvId::from_str(&eval_drv.drv_path)?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        insert_drv(&pool, &drv).await?;
        let jobs = [eval_drv];

        println!("creating jobset");
        let jobset_id = create_jobset(
            "abcdef",
            "fake-name",
            "test-owner",
            "test-repo",
            None,
            &pool,
        )
        .await?;
        create_jobs_for_jobset(jobset_id, &jobs[..], &pool).await?;

        // These two queries should return the same result if there's no jobset associated with the
        // base commit
        let jobs = jobs_for_jobset_id(jobset_id, &pool).await?;
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs.into_iter().next(), Some(drv.clone()));

        // Create a second jobset, ensure we get just one result back
        let drv2 = Drv {
            drv_path: DrvId::from_str(&eval_drv2.drv_path)?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        insert_drv(&pool, &drv2).await?;
        let jobs = [eval_drv2];

        let second_jobset_id = create_jobset(
            "g1cdef",
            "fake-name",
            "test-owner",
            "test-repo",
            None,
            &pool,
        )
        .await?;
        create_jobs_for_jobset(second_jobset_id, &jobs[..], &pool).await?;
        let (_, changed_jobs, _) = job_difference("abcdef", "g1cdef", "fake-name", &pool).await?;
        assert_eq!(changed_jobs.len(), 1);
        assert_eq!(changed_jobs.into_iter().next(), Some(drv));

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_create_jobs_for_jobset_empty(pool: SqlitePool) -> anyhow::Result<()> {
        // Create a jobset
        let jobset_id = create_jobset(
            "test-sha",
            "test-job",
            "test-owner",
            "test-repo",
            None,
            &pool,
        )
        .await?;

        // Call with empty jobs list - should not error
        let empty_jobs: Vec<NixEvalDrv> = vec![];
        create_jobs_for_jobset(jobset_id, &empty_jobs, &pool).await?;

        // Verify no jobs were created
        let jobs = jobs_for_jobset_id(jobset_id, &pool).await?;
        assert_eq!(jobs.len(), 0);

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_create_jobs_for_jobset_multiple_jobs_batch(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        use std::str::FromStr;

        // Create multiple test drvs and eval_drvs
        let drv_paths = vec![
            "/nix/store/1fr8b3xlygv2a64ff7fq7564j4sxv4lc-package1.drv",
            "/nix/store/2fr8b3xlygv2a64ff7fq7564j4sxv4lc-package2.drv",
            "/nix/store/3fr8b3xlygv2a64ff7fq7564j4sxv4lc-package3.drv",
            "/nix/store/4fr8b3xlygv2a64ff7fq7564j4sxv4lc-package4.drv",
        ];

        let attrs = vec!["package1", "package2", "package3", "package4"];

        // Insert the Drvs first
        for (drv_path, _) in drv_paths.iter().zip(attrs.iter()) {
            let drv = Drv {
                drv_path: DrvId::from_str(drv_path)?,
                system: "x86_64-linux".to_string(),
                prefer_local_build: false,
                required_system_features: None,
                is_fod: false,
                build_state: DrvBuildState::Queued,
                output_size: None,
                closure_size: None,
                pname: None,
                version: None,
                license_json: None,
                maintainers_json: None,
                meta_position: None,
                broken: None,
                insecure: None,
            };
            insert_drv(&pool, &drv).await?;
        }

        // Create NixEvalDrv objects
        let eval_drvs: Vec<NixEvalDrv> = drv_paths
            .iter()
            .zip(attrs.iter())
            .map(|(drv_path, attr)| NixEvalDrv {
                attr: attr.to_string(),
                attr_path: vec![attr.to_string()],
                drv_path: drv_path.to_string(),
                input_drvs: None,
                name: format!("{}-1.0.0", attr),
                system: "x86_64-linux".to_string(),
                outputs: std::collections::HashMap::new(),
                meta: None,
            })
            .collect();

        // Create a jobset and insert all jobs in one batch
        let jobset_id = create_jobset(
            "batch-sha",
            "batch-job",
            "test-owner",
            "test-repo",
            None,
            &pool,
        )
        .await?;
        create_jobs_for_jobset(jobset_id, &eval_drvs, &pool).await?;

        // Verify all jobs were created
        let jobs = jobs_for_jobset_id(jobset_id, &pool).await?;
        assert_eq!(jobs.len(), 4);

        // Verify all drv_paths are present
        let retrieved_paths: std::collections::HashSet<_> =
            jobs.iter().map(|j| j.drv_path.clone()).collect();
        for drv_path in &drv_paths {
            assert!(retrieved_paths.contains(&DrvId::from_str(drv_path)?));
        }

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_create_jobs_for_jobset_verifies_relationships(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        use std::str::FromStr;

        let drv_path = "/nix/store/5fr8b3xlygv2a64ff7fq7564j4sxv4lc-test-package.drv";
        let attr = "test.package";

        // Insert the Drv first
        let drv = Drv {
            drv_path: DrvId::from_str(drv_path)?,
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        insert_drv(&pool, &drv).await?;

        // Create NixEvalDrv
        let eval_drv = NixEvalDrv {
            attr: attr.to_string(),
            attr_path: vec!["test".to_string(), "package".to_string()],
            drv_path: drv_path.to_string(),
            input_drvs: None,
            name: "test-package-1.0.0".to_string(),
            system: "x86_64-linux".to_string(),
            outputs: std::collections::HashMap::new(),
            meta: None,
        };

        // Create a jobset and insert the job
        let jobset_id =
            create_jobset("rel-sha", "rel-job", "rel-owner", "rel-repo", None, &pool).await?;
        create_jobs_for_jobset(jobset_id, &[eval_drv], &pool).await?;

        // Query the Job table directly to verify relationships
        #[derive(sqlx::FromRow)]
        struct JobRecord {
            jobset: i64,
            drv_id: i64,
            name: String,
        }

        let job: JobRecord =
            sqlx::query_as("SELECT jobset, drv_id, name FROM Job WHERE jobset = ? AND name = ?")
                .bind(jobset_id)
                .bind(attr)
                .fetch_one(&pool)
                .await?;

        // Verify jobset FK is correct
        assert_eq!(job.jobset, jobset_id);

        // Verify the name (attr) is stored correctly
        assert_eq!(job.name, attr);

        // Verify drv_id points to the correct Drv
        let drv_rowid: i64 = sqlx::query_scalar("SELECT ROWID FROM Drv WHERE drv_path = ?")
            .bind(&DrvId::from_str(drv_path)?)
            .fetch_one(&pool)
            .await?;
        assert_eq!(job.drv_id, drv_rowid);

        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_create_jobs_for_jobset_large_batch(pool: SqlitePool) -> anyhow::Result<()> {
        use std::str::FromStr;

        // Create 15 test jobs to ensure batch insert scales
        let num_jobs = 15;
        let mut drv_paths = Vec::new();
        let mut attrs = Vec::new();

        // Generate unique drv paths and attributes
        // Base hash: 3fr8b3xlygv2a64ff7fq7564j4sxv4lc (32 chars)
        // We'll vary the first two characters to create unique hashes
        let base_hashes = vec![
            "0fr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "1fr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "2fr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "3fr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "4fr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "5fr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "6fr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "7fr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "8fr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "9fr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "afr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "bfr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "cfr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "dfr8b3xlygv2a64ff7fq7564j4sxv4lc",
            "ffr8b3xlygv2a64ff7fq7564j4sxv4lc",
        ];

        for (i, hash) in base_hashes.iter().enumerate() {
            let drv_path = format!("/nix/store/{}-pkg{}.drv", hash, i);
            drv_paths.push(drv_path);
            attrs.push(format!("packages.pkg{}", i));
        }

        // Insert all Drvs
        for drv_path in &drv_paths {
            let drv = Drv {
                drv_path: DrvId::from_str(drv_path)?,
                system: "x86_64-linux".to_string(),
                prefer_local_build: false,
                required_system_features: None,
                is_fod: false,
                build_state: DrvBuildState::Queued,
                output_size: None,
                closure_size: None,
                pname: None,
                version: None,
                license_json: None,
                maintainers_json: None,
                meta_position: None,
                broken: None,
                insecure: None,
            };
            insert_drv(&pool, &drv).await?;
        }

        // Create NixEvalDrv objects
        let eval_drvs: Vec<NixEvalDrv> = drv_paths
            .iter()
            .zip(attrs.iter())
            .map(|(drv_path, attr)| NixEvalDrv {
                attr: attr.to_string(),
                attr_path: vec![attr.to_string()],
                drv_path: drv_path.to_string(),
                input_drvs: None,
                name: format!("{}-1.0.0", attr),
                system: "x86_64-linux".to_string(),
                outputs: std::collections::HashMap::new(),
                meta: None,
            })
            .collect();

        // Create a jobset and insert all jobs in one large batch
        let jobset_id = create_jobset(
            "large-sha",
            "large-job",
            "test-owner",
            "test-repo",
            None,
            &pool,
        )
        .await?;
        create_jobs_for_jobset(jobset_id, &eval_drvs, &pool).await?;

        // Verify all jobs were created
        let jobs = jobs_for_jobset_id(jobset_id, &pool).await?;
        assert_eq!(jobs.len(), num_jobs);

        // Verify all drv_paths are present
        let retrieved_paths: std::collections::HashSet<_> =
            jobs.iter().map(|j| j.drv_path.clone()).collect();
        assert_eq!(retrieved_paths.len(), num_jobs);

        Ok(())
    }

    // ---- pr_head_build_succeeded test helpers ----

    use crate::db::model::build_event::DrvBuildResult;

    /// Insert a drv row, then overwrite its `build_state` to the desired value.
    /// `insert_drv` always initializes to `Queued`, which is unhelpful for the
    /// terminal-state scenarios below.
    async fn insert_drv_with_state(
        pool: &SqlitePool,
        drv_path: &str,
        state: DrvBuildState,
    ) -> anyhow::Result<DrvId> {
        use std::str::FromStr;

        let drv_id = DrvId::from_str(drv_path)?;
        let drv = Drv {
            drv_path: drv_id.clone(),
            system: "x86_64-linux".to_string(),
            prefer_local_build: false,
            required_system_features: None,
            is_fod: false,
            build_state: DrvBuildState::Queued,
            output_size: None,
            closure_size: None,
            pname: None,
            version: None,
            license_json: None,
            maintainers_json: None,
            meta_position: None,
            broken: None,
            insecure: None,
        };
        insert_drv(pool, &drv).await?;

        sqlx::query("UPDATE Drv SET build_state = ? WHERE drv_path = ?")
            .bind(state)
            .bind(&drv_id)
            .execute(pool)
            .await?;

        Ok(drv_id)
    }

    fn eval_drv_for(drv_path: &str, attr: &str) -> NixEvalDrv {
        NixEvalDrv {
            attr: attr.to_string(),
            attr_path: vec![attr.to_string()],
            drv_path: drv_path.to_string(),
            input_drvs: None,
            name: format!("{}-1.0.0", attr),
            system: "x86_64-linux".to_string(),
            outputs: std::collections::HashMap::new(),
            meta: None,
        }
    }

    async fn insert_test_pr(pool: &SqlitePool) -> anyhow::Result<()> {
        upsert_pull_request(
            42,
            "owner",
            "repo",
            "headsha",
            "basesha",
            "test pr",
            "author",
            "open",
            "2025-01-01T00:00:00Z",
            "2025-01-01T00:00:00Z",
            pool,
        )
        .await
    }

    /// Case 1: the PR exists but no jobset has been recorded for its head sha.
    /// The function must not treat "nothing known" as "built successfully".
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_pr_head_build_succeeded_no_jobset_returns_false(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        insert_test_pr(&pool).await?;

        // Intentionally do NOT create a GitHubJobSets row for "headsha".
        let result = pr_head_build_succeeded(42, "owner", "repo", &pool).await?;
        assert!(
            !result,
            "expected false when no jobset exists for the PR's head sha"
        );
        Ok(())
    }

    /// Case 2: a jobset exists but at least one job is still in a non-terminal
    /// state (e.g. Building). Auto-merge must wait.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_pr_head_build_succeeded_non_terminal_returns_false(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        insert_test_pr(&pool).await?;

        let drv_done = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg1.drv";
        let drv_building = "/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-pkg2.drv";
        insert_drv_with_state(
            &pool,
            drv_done,
            DrvBuildState::Completed(DrvBuildResult::Success),
        )
        .await?;
        insert_drv_with_state(&pool, drv_building, DrvBuildState::Building).await?;

        let jobset_id = create_jobset("headsha", "ci", "owner", "repo", None, &pool).await?;
        create_jobs_for_jobset(
            jobset_id,
            &[
                eval_drv_for(drv_done, "pkg1"),
                eval_drv_for(drv_building, "pkg2"),
            ],
            &pool,
        )
        .await?;

        let result = pr_head_build_succeeded(42, "owner", "repo", &pool).await?;
        assert!(
            !result,
            "expected false when a job is still in a non-terminal state"
        );
        Ok(())
    }

    /// Case 3: every job has concluded, but a new/changed job ended in a
    /// failure state. `jobset_has_new_or_changed_failures` should veto the
    /// merge.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_pr_head_build_succeeded_failure_returns_false(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        insert_test_pr(&pool).await?;

        let drv_ok = "/nix/store/cccccccccccccccccccccccccccccccc-pkg1.drv";
        let drv_fail = "/nix/store/dddddddddddddddddddddddddddddddd-pkg2.drv";
        insert_drv_with_state(
            &pool,
            drv_ok,
            DrvBuildState::Completed(DrvBuildResult::Success),
        )
        .await?;
        insert_drv_with_state(
            &pool,
            drv_fail,
            DrvBuildState::Completed(DrvBuildResult::Failure),
        )
        .await?;

        let jobset_id = create_jobset("headsha", "ci", "owner", "repo", None, &pool).await?;
        create_jobs_for_jobset(
            jobset_id,
            &[eval_drv_for(drv_ok, "pkg1"), eval_drv_for(drv_fail, "pkg2")],
            &pool,
        )
        .await?;
        // `Job.difference` defaults to 0 (New) in the schema, so the failing
        // job naturally counts as a "new or changed failure".

        let result = pr_head_build_succeeded(42, "owner", "repo", &pool).await?;
        assert!(
            !result,
            "expected false when a new/changed job ended in failure"
        );
        Ok(())
    }

    /// Case 4: every job has concluded successfully. This is the only case
    /// where the function is allowed to return true and unblock auto-merge.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn test_pr_head_build_succeeded_all_success_returns_true(
        pool: SqlitePool,
    ) -> anyhow::Result<()> {
        insert_test_pr(&pool).await?;

        // Hashes must be 32 chars from nix base32 (no e/o/t/u).
        let drv1 = "/nix/store/gggggggggggggggggggggggggggggggg-pkg1.drv";
        let drv2 = "/nix/store/ffffffffffffffffffffffffffffffff-pkg2.drv";
        insert_drv_with_state(
            &pool,
            drv1,
            DrvBuildState::Completed(DrvBuildResult::Success),
        )
        .await?;
        insert_drv_with_state(
            &pool,
            drv2,
            DrvBuildState::Completed(DrvBuildResult::Success),
        )
        .await?;

        let jobset_id = create_jobset("headsha", "ci", "owner", "repo", None, &pool).await?;
        create_jobs_for_jobset(
            jobset_id,
            &[eval_drv_for(drv1, "pkg1"), eval_drv_for(drv2, "pkg2")],
            &pool,
        )
        .await?;

        let result = pr_head_build_succeeded(42, "owner", "repo", &pool).await?;
        assert!(result, "expected true when all jobs concluded successfully");
        Ok(())
    }
}
