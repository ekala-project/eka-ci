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
        self.send_gh_update_with_log(octocrab, status, None).await
    }

    pub async fn send_gh_update_with_log(
        &self,
        octocrab: &Octocrab,
        status: &DrvBuildState,
        log_tail: Option<&str>,
    ) -> Result<GHCheckRun> {
        let (gh_status, gh_conclusion) = status.as_gh_checkrun_state();

        let check_builder = octocrab.checks(&self.repo_owner, &self.repo_name);
        let mut check_update = check_builder
            .update_check_run(octocrab::models::CheckRunId(self.check_run_id as u64))
            .status(gh_status);

        if let Some(conclusion) = gh_conclusion {
            check_update = check_update.conclusion(conclusion);
        }

        if let Some(log) = log_tail {
            let output = octocrab::params::checks::CheckRunOutput {
                title: format!("{:?}", status),
                summary: format!("```\n{}\n```", log),
                text: None,
                annotations: vec![],
                images: vec![],
            };
            check_update = check_update.output(output);
        }

        let check_run = check_update.send().await?;
        Ok(check_run)
    }

    /// Update a check run with a custom output title and markdown summary.
    /// Used by coalesced gates to render variant status tables.
    pub async fn send_gh_update_with_summary(
        &self,
        octocrab: &Octocrab,
        status: &DrvBuildState,
        title: &str,
        summary_markdown: &str,
    ) -> Result<GHCheckRun> {
        let (gh_status, gh_conclusion) = status.as_gh_checkrun_state();

        let check_builder = octocrab.checks(&self.repo_owner, &self.repo_name);
        let mut check_update = check_builder
            .update_check_run(octocrab::models::CheckRunId(self.check_run_id as u64))
            .status(gh_status);

        if let Some(conclusion) = gh_conclusion {
            check_update = check_update.conclusion(conclusion);
        }

        let output = octocrab::params::checks::CheckRunOutput {
            title: title.to_string(),
            summary: summary_markdown.to_string(),
            text: None,
            annotations: vec![],
            images: vec![],
        };
        check_update = check_update.output(output);

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
    insert_check_run_info_with_node_id(check_run_id, drv_path, repo_name, repo_owner, None, pool)
        .await
}

/// Insert a new GitHubCheckRuns record with GraphQL node_id.
/// Idempotent: does nothing if the (check_run_id, drv_id) pair already exists.
pub async fn insert_check_run_info_with_node_id(
    check_run_id: i64,
    drv_path: &DrvId,
    repo_name: &str,
    repo_owner: &str,
    node_id: Option<&str>,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        INSERT INTO GitHubCheckRuns (check_run_id, drv_id, repo_name, repo_owner, node_id)
        VALUES (?, (SELECT ROWID FROM Drv WHERE drv_path = ? LIMIT 1), ?, ?, ?)
        ON CONFLICT (check_run_id, drv_id) DO NOTHING
        "#,
    )
    .bind(check_run_id)
    .bind(drv_path)
    .bind(repo_name)
    .bind(repo_owner)
    .bind(node_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Return all variant build states for a coalesced check run.
/// For non-coalesced check runs this returns a single row.
pub async fn variant_states_for_check_run(
    check_run_id: i64,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<super::VariantBuildState>> {
    let states = sqlx::query_as(
        r#"
        SELECT d.drv_path, j.name, d.build_state
        FROM GitHubCheckRuns c
        JOIN Drv d ON d.ROWID = c.drv_id
        JOIN Job j ON j.drv_id = d.ROWID
        WHERE c.check_run_id = ?
        "#,
    )
    .bind(check_run_id)
    .fetch_all(pool)
    .await?;

    Ok(states)
}

/// Find an existing coalesced check run for a group prefix within a jobset.
/// Used by the lazy failure path to attach new variants to an existing gate.
pub async fn find_coalesced_check_run_for_group(
    jobset_id: i64,
    group_prefix: &str,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Option<(i64, Option<String>)>> {
    let pattern = format!("{}.%", group_prefix);
    let result: Option<(i64, Option<String>)> = sqlx::query_as(
        r#"
        SELECT c.check_run_id, c.node_id
        FROM GitHubCheckRuns c
        JOIN Drv d ON d.ROWID = c.drv_id
        JOIN Job j ON j.drv_id = d.ROWID
        WHERE j.jobset = ? AND j.name LIKE ?
        LIMIT 1
        "#,
    )
    .bind(jobset_id)
    .bind(&pattern)
    .fetch_optional(pool)
    .await?;

    Ok(result)
}

/// Return all jobs in a group (by prefix) within a jobset.
/// Used to populate a coalesced gate when created lazily on first failure.
pub async fn get_group_jobs_in_jobset(
    jobset_id: i64,
    group_prefix: &str,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<super::NewOrChangedJob>> {
    let pattern = format!("{}.%", group_prefix);
    let jobs = sqlx::query_as(
        r#"
        SELECT j.name, j.difference, d.drv_path, d.build_state
        FROM Job j
        JOIN Drv d ON j.drv_id = d.ROWID
        WHERE j.jobset = ? AND j.name LIKE ?
        "#,
    )
    .bind(jobset_id)
    .bind(&pattern)
    .fetch_all(pool)
    .await?;

    Ok(jobs)
}

/// Return all checkruns which match a drv_path
pub async fn check_runs_for_drv_path(
    drv_path: &DrvId,
    pool: &Pool<Sqlite>,
) -> anyhow::Result<Vec<CheckRun>> {
    let check_runs = sqlx::query_as(
        r#"
        SELECT check_run_id, repo_name, repo_owner, build_state, drv_path, node_id
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
        SELECT DISTINCT c.check_run_id, c.repo_name, c.repo_owner, d.build_state, d.drv_path, c.node_id
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
