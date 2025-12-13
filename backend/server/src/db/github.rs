use anyhow::Result;
use octocrab::Octocrab;
use octocrab::models::checks::CheckRun as GHCheckRun;
use sqlx::{FromRow, Pool, Sqlite};

use super::model::build_event::DrvBuildState;
use super::model::{Drv, DrvId};
use crate::github::JobDifference;
use crate::nix::nix_eval_jobs::NixEvalDrv;

#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
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
    pool: &Pool<Sqlite>,
) -> Result<i64> {
    // Since the insert statement could be repetitive, we must separate inseration and rowid
    // selection
    sqlx::query("INSERT INTO GitHubJobSets (sha, job, owner, repo_name) VALUES (?, ?, ?, ?)")
        .bind(sha)
        .bind(name)
        .bind(owner)
        .bind(repo_name)
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

    // TODO: convert to using query builder
    for job in jobs {
        // Convert drv_path to DrvId
        let drv_id = DrvId::from_str(&job.drv_path)?;

        // Insert into DrvRefs table
        sqlx::query(
            "INSERT INTO Job (jobset, drv_id, name) VALUES (?, (SELECT rowid FROM Drv WHERE \
             drv_path = ? LIMIT 1), ?)",
        )
        .bind(jobset_id)
        .bind(drv_id)
        .bind(&job.attr)
        .execute(&mut *tx)
        .await?;
    }

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
        SELECT d.drv_path, d.system, d.required_system_features, d.build_state
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
        SELECT d.drv_path, d.system, d.required_system_features, d.build_state
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
    .bind(&head_jobset_id)
    .bind(&head_jobset_id)
    .bind(&base_jobset_id)
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
    .bind(&head_jobset_id)
    .bind(&base_jobset_id)
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
    if let None = maybe_base_jobset_id {
        let new_jobs = jobs_for_jobset_id(head_jobset_id, pool).await?;
        return Ok((new_jobs, Vec::new(), Vec::new()));
    }
    let base_jobset_id: i64 = maybe_base_jobset_id.unwrap();

    // Query for drvs which differ in drv_id but share the same job name
    let changed_drvs = sqlx::query_as(
        r#"
        SELECT d.drv_path, d.system, d.required_system_features, d.build_state
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
    .bind(&head_jobset_id)
    .bind(&base_jobset_id)
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

/// Check if all jobs in a jobset have reached a terminal state
/// Terminal states are: Completed (success or failure), TransitiveFailure, and Interrupted states
pub async fn all_jobs_concluded(jobset_id: i64, pool: &Pool<Sqlite>) -> anyhow::Result<bool> {
    // Query for jobs that are NOT in terminal states
    // Non-terminal states: Queued (0), Buildable (1), Building (7), Blocked (100)
    let non_terminal_count: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
        FROM Job j
        JOIN Drv d ON j.drv_id = d.ROWID
        WHERE j.jobset = ? AND d.build_state IN (0, 1, 7, 100)
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

pub async fn get_jobset_info(jobset_id: i64, pool: &Pool<Sqlite>) -> anyhow::Result<JobSetInfo> {
    let info =
        sqlx::query_as("SELECT sha, job, owner, repo_name FROM GitHubJobSets WHERE ROWID = ?")
            .bind(jobset_id)
            .fetch_one(pool)
            .await?;

    Ok(info)
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
            build_state: DrvBuildState::Queued,
        };
        insert_drv(&pool, &drv).await?;
        let jobs = [eval_drv];

        println!("creating jobset");
        let jobset_id =
            create_jobset("abcdef", "fake-name", "test-owner", "test-repo", &pool).await?;
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
            build_state: DrvBuildState::Queued,
        };
        insert_drv(&pool, &drv2).await?;
        let jobs = [eval_drv2];

        let second_jobset_id =
            create_jobset("g1cdef", "fake-name", "test-owner", "test-repo", &pool).await?;
        create_jobs_for_jobset(second_jobset_id, &jobs[..], &pool).await?;
        let (_, changed_jobs, _) = job_difference("abcdef", "g1cdef", "fake-name", &pool).await?;
        assert_eq!(changed_jobs.len(), 1);
        assert_eq!(changed_jobs.into_iter().next(), Some(drv));

        Ok(())
    }
}
