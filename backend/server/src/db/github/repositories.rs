// GitHub Repository and Commit operations

use anyhow::Result;
use sqlx::{Pool, Sqlite};

use super::super::model::build_event::DrvBuildState;
use super::types::{
    BuildingDrv, CommitInfo, CommitJob, JobSetDetails, JobSetDrv, RepositoryInfo,
    RepositoryJobSetSummary,
};

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

/// Get jobsets for a repository with build statistics and change summary counts
pub async fn get_repository_jobsets(
    owner: &str,
    repo_name: &str,
    limit: i64,
    sort_desc: bool,
    pool: &Pool<Sqlite>,
) -> Result<Vec<RepositoryJobSetSummary>> {
    let order = if sort_desc { "DESC" } else { "ASC" };
    let query = format!(
        r#"
        SELECT
            g.ROWID as jobset_id,
            g.job as job_name,
            g.sha,
            COUNT(DISTINCT j.drv_id) as total_drvs,
            SUM(CASE WHEN d.build_state = 0 THEN 1 ELSE 0 END) as queued_drvs,
            SUM(CASE WHEN d.build_state = 1 THEN 1 ELSE 0 END) as buildable_drvs,
            SUM(CASE WHEN d.build_state = 7 THEN 1 ELSE 0 END) as building_drvs,
            SUM(CASE WHEN d.build_state = 2 THEN 1 ELSE 0 END) as failed_retry_drvs,
            SUM(CASE WHEN d.build_state = 42 THEN 1 ELSE 0 END) as completed_success_drvs,
            SUM(CASE WHEN d.build_state = -1 THEN 1 ELSE 0 END) as completed_failure_drvs,
            SUM(CASE WHEN d.build_state = -2 THEN 1 ELSE 0 END) as transitive_failure_drvs,
            SUM(CASE WHEN d.build_state = 100 THEN 1 ELSE 0 END) as blocked_drvs,
            SUM(CASE WHEN d.build_state < 0 AND d.build_state != -1 AND d.build_state != -2 THEN 1 ELSE 0 END) as interrupted_drvs,
            SUM(CASE WHEN j.difference = 0 THEN 1 ELSE 0 END) as new_jobs,
            SUM(CASE WHEN j.difference = 1 THEN 1 ELSE 0 END) as changed_jobs,
            SUM(CASE WHEN j.difference = 2 THEN 1 ELSE 0 END) as removed_jobs
        FROM GitHubJobSets g
        JOIN Job j ON j.jobset = g.ROWID
        JOIN Drv d ON d.ROWID = j.drv_id
        WHERE g.owner = ? AND g.repo_name = ?
        GROUP BY g.ROWID
        ORDER BY g.ROWID {}
        LIMIT ?
        "#,
        order
    );

    let jobsets = sqlx::query_as(&query)
        .bind(owner)
        .bind(repo_name)
        .bind(limit)
        .fetch_all(pool)
        .await?;

    Ok(jobsets)
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
