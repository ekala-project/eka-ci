// GitHub Check Run operations

use anyhow::Result;
use octocrab::Octocrab;
use octocrab::models::checks::CheckRun as GHCheckRun;
use sqlx::{Pool, Sqlite};

use super::super::model::DrvId;
use super::super::model::build_event::DrvBuildState;
use super::types::CheckRun;

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

/// Insert a new GitHubCheckRuns record
pub async fn insert_check_run_info(
    check_run_id: i64,
    drv_path: &DrvId,
    repo_name: &str,
    repo_owner: &str,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO GitHubCheckRuns (check_run_id, drv_id, repo_name, repo_owner)
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
        FROM GitHubCheckRuns c
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
