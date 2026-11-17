// GitHub Jobset operations

use anyhow::Result;
use sqlx::{Pool, Sqlite};

use super::super::model::DrvId;
use super::types::{BaseJob, JobInfo, JobSetInfo, NewOrChangedJob};
use crate::github::JobDifference;
use crate::nix::NixEvalDrv;

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

/// A pull-request head jobset that was diffed against a base other than
/// `base_sha` (or before base SHAs were recorded).
///
/// Returns the stale jobset's id; `None` if there is no jobset or it was
/// diffed against `base_sha`.
pub async fn stale_head_jobset(
    sha: &str,
    name: &str,
    owner: &str,
    repo_name: &str,
    base_sha: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<i64>> {
    let id: Option<i64> = sqlx::query_scalar(
        "SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ? AND owner = ? AND repo_name = \
         ? AND base_sha IS NOT ?",
    )
    .bind(sha)
    .bind(name)
    .bind(owner)
    .bind(repo_name)
    .bind(base_sha)
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

/// ROWIDs of every jobset evaluated for `sha` in `owner/repo_name`
/// (one per configured job).
pub async fn head_jobset_ids(
    owner: &str,
    repo_name: &str,
    sha: &str,
    pool: &Pool<Sqlite>,
) -> Result<Vec<i64>> {
    let ids = sqlx::query_scalar(
        "SELECT ROWID FROM GitHubJobSets WHERE owner = ? AND repo_name = ? AND sha = ?",
    )
    .bind(owner)
    .bind(repo_name)
    .bind(sha)
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

/// Delete a jobset; its `Job` rows are removed by `ON DELETE CASCADE`.
pub async fn delete_jobset(jobset_id: i64, pool: &Pool<Sqlite>) -> Result<()> {
    sqlx::query("DELETE FROM GitHubJobSets WHERE ROWID = ?")
        .bind(jobset_id)
        .execute(pool)
        .await?;
    Ok(())
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

/// Record the base commit a pull-request jobset was evaluated against.
pub async fn set_jobset_base_sha(
    jobset_id: i64,
    base_sha: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query("UPDATE GitHubJobSets SET base_sha = ? WHERE ROWID = ?")
        .bind(base_sha)
        .bind(jobset_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// For `attr` in the pull-request jobset `jobset_id`, find the jobset of the
/// base commit (same job, owner and repo) and the derivation `attr` had there.
///
/// Returns `None` when the jobset has no recorded base commit, the base
/// commit was not evaluated, or the attribute is new in the pull request.
pub async fn get_base_drv_for_attr(
    jobset_id: i64,
    attr: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<(i64, DrvId)>> {
    let row = sqlx::query_as(
        r#"
        SELECT base.ROWID, d.drv_path
        FROM GitHubJobSets head
        JOIN GitHubJobSets base
          ON base.sha = head.base_sha
         AND base.job = head.job
         AND base.owner = head.owner
         AND base.repo_name = head.repo_name
        JOIN Job j ON j.jobset = base.ROWID AND j.name = ?
        JOIN Drv d ON d.ROWID = j.drv_id
        WHERE head.ROWID = ?
        LIMIT 1
        "#,
    )
    .bind(attr)
    .bind(jobset_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
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

    // Convert drv_paths to DrvIds and compute difference vs base.
    // Unchanged packages (same attr AND same drv_path) are excluded
    // entirely — only New and Changed packages get Job rows.
    // Note: base_map drv_paths are DrvId format (hash-name.drv) while
    // NixEvalDrv.drv_path is a full store path (/nix/store/...).
    // We parse job.drv_path into a DrvId for consistent comparison.
    let job_data: Vec<(DrvId, &str, i64)> = jobs
        .iter()
        .filter_map(|job| {
            let drv_id = DrvId::from_str(&job.drv_path).ok()?;
            let drv_id_str = drv_id.to_string();

            let difference = match base_map.get(job.attr.as_str()) {
                Some(base_drv_path) if *base_drv_path == drv_id_str => return None, // Unchanged
                Some(_) => 1, // Changed (different drv for same attr)
                None => 0,    // New (attr doesn't exist in base)
            };

            Some((drv_id, job.attr.as_str(), difference))
        })
        .collect();

    // Each row uses 4 bind variables (jobset, drv_path, attr, difference).
    // SQLite's default SQLITE_MAX_VARIABLE_NUMBER is 999, so we chunk at
    // 200 rows per INSERT to stay safely under the limit.
    const CHUNK_SIZE: usize = 200;

    for chunk in job_data.chunks(CHUNK_SIZE) {
        let mut query_builder =
            sqlx::QueryBuilder::new("INSERT INTO Job (jobset, drv_id, name, difference) VALUES ");

        for (i, (drv_id, attr, difference)) in chunk.iter().enumerate() {
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
    }

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

/// Get new or changed jobs for a jobset (difference = New or Changed).
/// Returns job info with the drv_id needed to create check_runs.
pub async fn get_new_or_changed_jobs(
    jobset_id: i64,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<NewOrChangedJob>> {
    let jobs = sqlx::query_as(
        r#"
        SELECT j.name, j.difference, d.drv_path, d.build_state
        FROM Job j
        JOIN Drv d ON j.drv_id = d.ROWID
        WHERE j.jobset = ? AND j.difference IN (?, ?)
        "#,
    )
    .bind(jobset_id)
    .bind(JobDifference::New)
    .bind(JobDifference::Changed)
    .fetch_all(pool)
    .await?;

    Ok(jobs)
}

pub async fn get_jobset_info(jobset_id: i64, pool: &Pool<Sqlite>) -> anyhow::Result<JobSetInfo> {
    let info =
        sqlx::query_as("SELECT sha, job, owner, repo_name FROM GitHubJobSets WHERE ROWID = ?")
            .bind(jobset_id)
            .fetch_one(pool)
            .await?;

    Ok(info)
}

/// Given a drv_path, find the jobset context needed to reconstitute it
/// after garbage collection. Returns the (sha, job, owner, repo_name)
/// for the first matching jobset that contains this drv as a direct job member.
pub async fn get_reconstitution_context(
    drv_id: &DrvId,
    pool: &Pool<Sqlite>,
) -> Result<Option<JobSetInfo>> {
    let info = sqlx::query_as(
        r#"
        SELECT g.sha, g.job, g.owner, g.repo_name
        FROM Job j
        JOIN GitHubJobSets g ON j.jobset = g.ROWID
        JOIN Drv d ON d.ROWID = j.drv_id
        WHERE d.drv_path = ?
        LIMIT 1
        "#,
    )
    .bind(drv_id)
    .fetch_optional(pool)
    .await?;

    Ok(info)
}

/// Given a drv_path for a transitive dependency (not directly in any Job),
/// walk the DrvRefs graph upward until we find an ancestor that IS in a
/// Job table, then return its jobset context. Re-evaluating that jobset's
/// nix file will reconstitute the entire dependency closure including the
/// target drv.
pub async fn get_reconstitution_context_transitive(
    drv_id: &DrvId,
    pool: &Pool<Sqlite>,
) -> Result<Option<JobSetInfo>> {
    let info = sqlx::query_as(
        r#"
        WITH RECURSIVE ancestors(drv_path) AS (
            SELECT ?1
            UNION
            SELECT dr.referrer FROM DrvRefs dr
            JOIN ancestors a ON dr.reference = a.drv_path
        )
        SELECT g.sha, g.job, g.owner, g.repo_name
        FROM ancestors a
        JOIN Drv d ON d.drv_path = a.drv_path
        JOIN Job j ON j.drv_id = d.ROWID
        JOIN GitHubJobSets g ON j.jobset = g.ROWID
        LIMIT 1
        "#,
    )
    .bind(drv_id)
    .fetch_optional(pool)
    .await?;

    Ok(info)
}

#[cfg(test)]
mod stale_head_tests {
    use sqlx::SqlitePool;

    use super::*;

    async fn head_jobset(pool: &SqlitePool, base: Option<&str>) -> Result<i64> {
        let id = create_jobset("head", "job", "o", "r", None, pool).await?;
        if let Some(base) = base {
            set_jobset_base_sha(id, base, pool).await?;
        }
        Ok(id)
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn same_base_is_not_stale(pool: SqlitePool) -> Result<()> {
        head_jobset(&pool, Some("base-a")).await?;
        let stale = stale_head_jobset("head", "job", "o", "r", "base-a", &pool).await?;
        assert_eq!(stale, None);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn other_or_unrecorded_base_is_stale(pool: SqlitePool) -> Result<()> {
        let id = head_jobset(&pool, Some("base-a")).await?;
        let stale = stale_head_jobset("head", "job", "o", "r", "base-b", &pool).await?;
        assert_eq!(stale, Some(id));

        delete_jobset(id, &pool).await?;
        let id = head_jobset(&pool, None).await?;
        let stale = stale_head_jobset("head", "job", "o", "r", "base-b", &pool).await?;
        assert_eq!(stale, Some(id));
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn delete_allows_recreating_the_jobset(pool: SqlitePool) -> Result<()> {
        let old = head_jobset(&pool, Some("base-a")).await?;
        delete_jobset(old, &pool).await?;
        assert!(!has_jobset("head", "job", "o", "r", &pool).await?);

        head_jobset(&pool, Some("base-b")).await?;
        let stale = stale_head_jobset("head", "job", "o", "r", "base-b", &pool).await?;
        assert_eq!(stale, None);
        Ok(())
    }
}
