//! Output and closure size persistence.
//!
//! Sizes measured after a build are written to the `Drv` row, to the
//! per-commit history tables, and, for derivations referenced by a
//! base-branch jobset, to `DrvSizeBaselines`. Size checks on PR builds look
//! the baseline up by attr path on the configured base branch, because the
//! drv path of a package changes whenever a PR changes it.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::db::model::DrvId;

/// Which size measurement a size check compares.
///
/// Journaled size-warning tasks written before closure sizes were checked
/// have no metric and deserialize as [`SizeMetric::Output`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeMetric {
    /// NAR size of the derivation's outputs.
    #[default]
    Output,
    /// Size of the outputs together with their runtime dependencies.
    Closure,
}

impl SizeMetric {
    /// Both metrics, in the order they are measured.
    pub const ALL: [SizeMetric; 2] = [SizeMetric::Output, SizeMetric::Closure];

    /// Human-readable name, e.g. for check-run titles.
    pub fn label(self) -> &'static str {
        match self {
            SizeMetric::Output => "Output size",
            SizeMetric::Closure => "Closure size",
        }
    }

    /// Column holding this metric in `Drv` and `DrvSizeBaselines`, and the
    /// size column of its history table.
    fn column(self) -> &'static str {
        match self {
            SizeMetric::Output => "output_size",
            SizeMetric::Closure => "closure_size",
        }
    }

    /// Per-commit history table for this metric.
    fn history_table(self) -> &'static str {
        match self {
            SizeMetric::Output => "DrvOutputSizes",
            SizeMetric::Closure => "DrvClosureSizes",
        }
    }
}

/// Identifies the base-branch baseline a PR build is compared against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaselineKey<'a> {
    /// Repository owner.
    pub owner: &'a str,
    /// Repository name.
    pub repo_name: &'a str,
    /// Base branch from the job's `size_check.base_branch`.
    pub branch: &'a str,
    /// Jobset job name (the key under `jobs` in `.ekaci/config.json`).
    pub job: &'a str,
    /// Attr path of the package (`Job.name`).
    pub attr_path: &'a str,
    /// System the derivation is built for.
    pub system: &'a str,
}

/// Look up the ROWID of a derivation in the `Drv` table.
///
/// `Drv.drv_path` stores the bare `hash-name.drv` form, which is what binding
/// a [`DrvId`] produces. Binding [`DrvId::store_path`] instead matches nothing.
pub async fn drv_rowid(pool: &SqlitePool, drv_id: &DrvId) -> Result<Option<i64>> {
    let rowid = sqlx::query_scalar("SELECT ROWID FROM Drv WHERE drv_path = ?")
        .bind(drv_id)
        .fetch_optional(pool)
        .await?;
    Ok(rowid)
}

/// Get the baseline size of `metric` for an attr on a base branch.
///
/// # Returns
/// * `Ok(Some(size))` - Latest size recorded for the attr on the branch
/// * `Ok(None)` - No base-branch build of this attr has a recorded size yet
pub async fn get_baseline_size(
    pool: &SqlitePool,
    key: &BaselineKey<'_>,
    metric: SizeMetric,
) -> Result<Option<u64>> {
    let query = format!(
        "SELECT {} FROM DrvSizeBaselines WHERE owner = ? AND repo_name = ? AND branch = ? AND \
         job = ? AND attr_path = ? AND system = ?",
        metric.column()
    );
    let size: Option<Option<i64>> = sqlx::query_scalar(&query)
        .bind(key.owner)
        .bind(key.repo_name)
        .bind(key.branch)
        .bind(key.job)
        .bind(key.attr_path)
        .bind(key.system)
        .fetch_optional(pool)
        .await?;

    Ok(size.flatten().map(|s| s as u64))
}

/// Copy recorded sizes from `Drv` rows into `DrvSizeBaselines` for a
/// base-branch jobset.
///
/// Does nothing unless the jobset has a branch (see [`set_jobset_branch`]).
/// When `drv_id` is given, only that derivation's jobs are copied; otherwise
/// every job of the jobset whose derivation has a recorded size is. Existing
/// baselines for the same attr are replaced; a metric the new derivation has
/// no size for keeps its previous baseline.
///
/// Returns the number of baselines written.
pub async fn record_size_baselines(
    pool: &SqlitePool,
    jobset_id: i64,
    drv_id: Option<&DrvId>,
) -> Result<u64> {
    let result = sqlx::query(
        r#"
        INSERT INTO DrvSizeBaselines
            (owner, repo_name, branch, job, attr_path, system, drv_path, git_commit,
             output_size, closure_size)
        SELECT g.owner, g.repo_name, g.branch, g.job, j.name, d.system, d.drv_path, g.sha,
               d.output_size, d.closure_size
        FROM Job j
        INNER JOIN GitHubJobSets g ON g.ROWID = j.jobset
        INNER JOIN Drv d ON d.ROWID = j.drv_id
        WHERE g.ROWID = ?
          AND g.branch IS NOT NULL
          AND (d.output_size IS NOT NULL OR d.closure_size IS NOT NULL)
          AND (? IS NULL OR d.drv_path = ?)
        ON CONFLICT (owner, repo_name, branch, job, attr_path, system) DO UPDATE SET
            drv_path = excluded.drv_path,
            git_commit = excluded.git_commit,
            output_size = COALESCE(excluded.output_size, output_size),
            closure_size = COALESCE(excluded.closure_size, closure_size),
            recorded_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(jobset_id)
    .bind(drv_id)
    .bind(drv_id)
    .execute(pool)
    .await?;

    Ok(result.rows_affected())
}

/// Mark a jobset as the evaluation of `branch`'s tip.
///
/// A branch already recorded for the jobset is kept.
pub async fn set_jobset_branch(pool: &SqlitePool, jobset_id: i64, branch: &str) -> Result<()> {
    sqlx::query("UPDATE GitHubJobSets SET branch = ? WHERE ROWID = ? AND branch IS NULL")
        .bind(branch)
        .bind(jobset_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Record a measured size in the per-commit history table of `metric`.
///
/// # Arguments
/// * `pool` - Database connection pool
/// * `drv_id` - The derivation
/// * `metric` - Which size `size` is
/// * `size` - Size in bytes
/// * `git_commit` - Commit SHA this build corresponds to
/// * `git_repo` - Repository URL
pub async fn store_size_history(
    pool: &SqlitePool,
    drv_id: &DrvId,
    metric: SizeMetric,
    size: u64,
    git_commit: &str,
    git_repo: &str,
) -> Result<()> {
    let (table, column) = (metric.history_table(), metric.column());
    let query = format!(
        "INSERT INTO {table} (drv_path, {column}, git_commit, git_repo) VALUES (?, ?, ?, ?) ON \
         CONFLICT(drv_path, git_commit, git_repo) DO UPDATE SET {column} = excluded.{column}, \
         recorded_at = CURRENT_TIMESTAMP"
    );
    sqlx::query(&query)
        .bind(drv_id)
        .bind(size as i64)
        .bind(git_commit)
        .bind(git_repo)
        .execute(pool)
        .await?;

    Ok(())
}

/// Set the `metric` column of the derivation's `Drv` row.
pub async fn update_drv_size(
    pool: &SqlitePool,
    drv_id: &DrvId,
    metric: SizeMetric,
    size: u64,
) -> Result<()> {
    let query = format!("UPDATE Drv SET {} = ? WHERE drv_path = ?", metric.column());
    sqlx::query(&query)
        .bind(size as i64)
        .bind(drv_id)
        .execute(pool)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::db::github::{create_jobs_for_jobset, create_jobset};
    use crate::db::model::build_event::DrvBuildState;
    use crate::db::model::drv::{Drv, insert_drv};
    use crate::nix::NixEvalDrv;

    const BASE_DRV: &str = "/nix/store/jd83l3jn2mkn530lgcg0y523jq5qji85-hello-2.12.1.drv";
    const HEAD_DRV: &str = "/nix/store/gciipqhqkdlqqn803zd4a389v86ran45-hello-2.12.2.drv";
    const OUT: SizeMetric = SizeMetric::Output;
    const CLOSURE: SizeMetric = SizeMetric::Closure;

    async fn insert_test_drv(pool: &SqlitePool, drv_path: &str) -> Result<DrvId> {
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
        Ok(drv_id)
    }

    /// Create a jobset for `sha` with a single `hello` job pointing at `drv_path`.
    async fn jobset_with_hello(pool: &SqlitePool, sha: &str, drv_path: &str) -> Result<i64> {
        let jobset_id = create_jobset(sha, "packages", "owner", "repo", None, pool).await?;
        let job = NixEvalDrv {
            attr: "hello".to_string(),
            attr_path: vec!["hello".to_string()],
            drv_path: drv_path.to_string(),
            input_drvs: Default::default(),
            name: "hello".to_string(),
            outputs: Default::default(),
            system: "x86_64-linux".to_string(),
            meta: None,
            pos: None,
        };
        create_jobs_for_jobset(jobset_id, &[job], None, pool).await?;
        Ok(jobset_id)
    }

    fn hello_key(branch: &str) -> BaselineKey<'_> {
        BaselineKey {
            owner: "owner",
            repo_name: "repo",
            branch,
            job: "packages",
            attr_path: "hello",
            system: "x86_64-linux",
        }
    }

    async fn drv_output_size(pool: &SqlitePool, drv_id: &DrvId) -> Result<Option<i64>> {
        let size = sqlx::query_scalar("SELECT output_size FROM Drv WHERE drv_path = ?")
            .bind(drv_id)
            .fetch_one(pool)
            .await?;
        Ok(size)
    }

    /// Bug 1: `Drv.drv_path` holds `hash-name.drv`, so binding the full
    /// `/nix/store/...` path (what the recorder used to do) matches no row.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn drv_row_is_found_by_drv_id_not_store_path(pool: SqlitePool) -> Result<()> {
        let drv_id = insert_test_drv(&pool, BASE_DRV).await?;

        let by_store_path: Option<i64> =
            sqlx::query_scalar("SELECT ROWID FROM Drv WHERE drv_path = ?")
                .bind(drv_id.store_path())
                .fetch_optional(&pool)
                .await?;
        assert_eq!(by_store_path, None, "store path form must not match");

        assert!(drv_rowid(&pool, &drv_id).await?.is_some());

        update_drv_size(&pool, &drv_id, OUT, 4096).await?;
        assert_eq!(drv_output_size(&pool, &drv_id).await?, Some(4096));
        Ok(())
    }

    /// Bug 2: the old baseline query inner-joined `DrvBuildMetadata`, which
    /// nothing writes to. The baseline must be found without it.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn baseline_does_not_need_build_metadata(pool: SqlitePool) -> Result<()> {
        let base_id = insert_test_drv(&pool, BASE_DRV).await?;
        let base_jobset = jobset_with_hello(&pool, "base-sha", BASE_DRV).await?;
        update_drv_size(&pool, &base_id, OUT, 1000).await?;
        set_jobset_branch(&pool, base_jobset, "main").await?;
        assert_eq!(record_size_baselines(&pool, base_jobset, None).await?, 1);

        let metadata_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM DrvBuildMetadata")
            .fetch_one(&pool)
            .await?;
        assert_eq!(metadata_rows, 0);
        assert_eq!(
            get_baseline_size(&pool, &hello_key("main"), OUT).await?,
            Some(1000)
        );
        Ok(())
    }

    /// Bug 3: a PR that changes a package has a new drv path, so the
    /// baseline is looked up by attr path on the base branch instead.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn baseline_is_keyed_by_attr_not_drv_path(pool: SqlitePool) -> Result<()> {
        let base_id = insert_test_drv(&pool, BASE_DRV).await?;
        let base_jobset = jobset_with_hello(&pool, "base-sha", BASE_DRV).await?;
        update_drv_size(&pool, &base_id, OUT, 1000).await?;
        set_jobset_branch(&pool, base_jobset, "main").await?;
        record_size_baselines(&pool, base_jobset, None).await?;

        // The head build of the same attr is a different derivation.
        insert_test_drv(&pool, HEAD_DRV).await?;
        jobset_with_hello(&pool, "head-sha", HEAD_DRV).await?;

        assert_eq!(
            get_baseline_size(&pool, &hello_key("main"), OUT).await?,
            Some(1000)
        );
        assert_eq!(
            get_baseline_size(&pool, &hello_key("dev"), OUT).await?,
            None
        );
        let other_system = BaselineKey {
            system: "aarch64-linux",
            ..hello_key("main")
        };
        assert_eq!(get_baseline_size(&pool, &other_system, OUT).await?, None);
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn baselines_need_a_branch_and_a_size(pool: SqlitePool) -> Result<()> {
        let base_id = insert_test_drv(&pool, BASE_DRV).await?;
        let jobset = jobset_with_hello(&pool, "base-sha", BASE_DRV).await?;

        // No size yet, then no branch: nothing to record either way.
        set_jobset_branch(&pool, jobset, "main").await?;
        assert_eq!(record_size_baselines(&pool, jobset, None).await?, 0);

        let head_id = insert_test_drv(&pool, HEAD_DRV).await?;
        let head_jobset = jobset_with_hello(&pool, "head-sha", HEAD_DRV).await?;
        update_drv_size(&pool, &head_id, OUT, 10).await?;
        assert_eq!(record_size_baselines(&pool, head_jobset, None).await?, 0);

        // A later base build replaces the baseline for the attr.
        update_drv_size(&pool, &base_id, OUT, 2000).await?;
        assert_eq!(
            record_size_baselines(&pool, jobset, Some(&base_id)).await?,
            1
        );
        assert_eq!(
            get_baseline_size(&pool, &hello_key("main"), OUT).await?,
            Some(2000)
        );
        Ok(())
    }

    /// Closure sizes share the baseline row (and key) of the output size.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn closure_baseline_shares_the_attr_key(pool: SqlitePool) -> Result<()> {
        let base_id = insert_test_drv(&pool, BASE_DRV).await?;
        let jobset = jobset_with_hello(&pool, "base-sha", BASE_DRV).await?;
        set_jobset_branch(&pool, jobset, "main").await?;
        update_drv_size(&pool, &base_id, CLOSURE, 50_000).await?;
        assert_eq!(record_size_baselines(&pool, jobset, None).await?, 1);
        assert_eq!(
            get_baseline_size(&pool, &hello_key("main"), OUT).await?,
            None
        );
        update_drv_size(&pool, &base_id, OUT, 1000).await?;
        record_size_baselines(&pool, jobset, Some(&base_id)).await?;

        let key = hello_key("main");
        assert_eq!(get_baseline_size(&pool, &key, CLOSURE).await?, Some(50_000));
        assert_eq!(get_baseline_size(&pool, &key, OUT).await?, Some(1000));
        Ok(())
    }

    /// A newer base build whose closure was not measured keeps the previous
    /// closure baseline instead of clearing it.
    #[sqlx::test(migrations = "./sql/migrations")]
    async fn unmeasured_metric_keeps_previous_baseline(pool: SqlitePool) -> Result<()> {
        let old_id = insert_test_drv(&pool, BASE_DRV).await?;
        let old = jobset_with_hello(&pool, "old-sha", BASE_DRV).await?;
        set_jobset_branch(&pool, old, "main").await?;
        update_drv_size(&pool, &old_id, OUT, 1000).await?;
        update_drv_size(&pool, &old_id, CLOSURE, 50_000).await?;
        record_size_baselines(&pool, old, None).await?;

        let new_id = insert_test_drv(&pool, HEAD_DRV).await?;
        let new = jobset_with_hello(&pool, "new-sha", HEAD_DRV).await?;
        set_jobset_branch(&pool, new, "main").await?;
        update_drv_size(&pool, &new_id, OUT, 1200).await?;
        record_size_baselines(&pool, new, None).await?;

        let key = hello_key("main");
        assert_eq!(get_baseline_size(&pool, &key, OUT).await?, Some(1200));
        assert_eq!(get_baseline_size(&pool, &key, CLOSURE).await?, Some(50_000));
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn size_history_goes_to_the_metric_table(pool: SqlitePool) -> Result<()> {
        let drv_id = DrvId::from_str(BASE_DRV)?;
        store_size_history(&pool, &drv_id, OUT, 10, "sha", "repo").await?;
        store_size_history(&pool, &drv_id, CLOSURE, 30, "sha", "repo").await?;
        store_size_history(&pool, &drv_id, CLOSURE, 40, "sha", "repo").await?;

        let output: i64 = sqlx::query_scalar("SELECT output_size FROM DrvOutputSizes")
            .fetch_one(&pool)
            .await?;
        let closure: Vec<i64> = sqlx::query_scalar("SELECT closure_size FROM DrvClosureSizes")
            .fetch_all(&pool)
            .await?;
        assert_eq!((output, closure), (10, vec![40]));
        Ok(())
    }

    #[sqlx::test(migrations = "./sql/migrations")]
    async fn jobset_branch_is_not_overwritten(pool: SqlitePool) -> Result<()> {
        let jobset = create_jobset("sha", "packages", "owner", "repo", None, &pool).await?;
        set_jobset_branch(&pool, jobset, "main").await?;
        set_jobset_branch(&pool, jobset, "other").await?;
        let branch: Option<String> =
            sqlx::query_scalar("SELECT branch FROM GitHubJobSets WHERE ROWID = ?")
                .bind(jobset)
                .fetch_one(&pool)
                .await?;
        assert_eq!(branch.as_deref(), Some("main"));
        Ok(())
    }
}
