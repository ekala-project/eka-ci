//! Fallback baselines from a pull request's base commit.
//!
//! Base-branch baselines are recorded from derivations eka-ci measured: ones
//! it built, as a PR head or on the base branch. A changed attr whose base
//! derivation eka-ci never built has no baseline. For those, measure the
//! base commit's derivation where its outputs already are, in the local store
//! or a substituter (only `.narinfo` files are fetched), store the sizes on
//! its `Drv` row and record them as the base jobset's baseline.

use tracing::{debug, warn};

use super::SizeChecker;
use crate::db::github::{JobInfo, get_base_drv_for_attr};
use crate::db::model::DrvId;
use crate::db::size::{self, SizeMetric};
use crate::nix::size::{OutputSizeReport, configured_substituters, measure_outputs};

impl SizeChecker {
    /// Make sure the base commit's derivation of `job_info.name` has recorded
    /// sizes and is recorded as its base jobset's baseline.
    ///
    /// Does nothing for jobs without `size_check`, for jobsets that are not
    /// pull-request heads, and when the base derivation was measured before.
    pub(super) async fn ensure_base_commit_baseline(
        &self,
        job_info: &JobInfo,
    ) -> anyhow::Result<()> {
        if super::size_eval::job_size_check(&self.pool, job_info.jobset_id)
            .await?
            .is_none()
        {
            return Ok(());
        }
        let base = get_base_drv_for_attr(job_info.jobset_id, &job_info.name, &self.pool).await?;
        let Some((base_jobset, base_drv)) = base else {
            return Ok(());
        };
        if !has_both_sizes(&self.pool, &base_drv).await? {
            let Some(report) = self.measure_unbuilt(&base_drv).await else {
                return Ok(());
            };
            let pool = &self.pool;
            size::update_drv_size(pool, &base_drv, SizeMetric::Output, report.total_nar_size)
                .await?;
            size::update_drv_size(
                pool,
                &base_drv,
                SizeMetric::Closure,
                report.total_closure_size,
            )
            .await?;
        }
        size::record_size_baselines(&self.pool, base_jobset, Some(&base_drv)).await?;
        Ok(())
    }

    /// Measure a derivation eka-ci did not build: try the local store, then
    /// each configured substituter.
    async fn measure_unbuilt(&self, drv: &DrvId) -> Option<OutputSizeReport> {
        let outputs = match crate::nix::get_drv_outputs(&drv.store_path()).await {
            Ok(outputs) if !outputs.is_empty() => outputs,
            Ok(_) => return None,
            Err(e) => {
                debug!("Cannot read base derivation {}: {}", drv.store_path(), e);
                return None;
            },
        };
        let paths: Vec<String> = outputs.into_values().collect();

        let mut stores: Vec<Option<String>> = vec![None];
        stores.extend(self.substituters().await.iter().cloned().map(Some));
        for store in stores {
            match measure_outputs(&paths, store.as_deref()).await {
                Ok(report) => return Some(report),
                Err(e) => debug!(
                    "Base {} not measurable in {}: {}",
                    drv.store_path(),
                    store.as_deref().unwrap_or("local store"),
                    e
                ),
            }
        }
        debug!("No store has the outputs of base {}", drv.store_path());
        None
    }

    /// Substituters of the local Nix configuration, queried once.
    async fn substituters(&self) -> &[String] {
        self.substituters
            .get_or_init(|| async {
                configured_substituters().await.unwrap_or_else(|e| {
                    warn!("Cannot list substituters for size baselines: {}", e);
                    Vec::new()
                })
            })
            .await
    }
}

/// Whether the derivation's `Drv` row has both an output and a closure size.
async fn has_both_sizes(pool: &sqlx::SqlitePool, drv_id: &DrvId) -> anyhow::Result<bool> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM Drv WHERE drv_path = ? AND output_size IS NOT NULL AND closure_size \
         IS NOT NULL",
    )
    .bind(drv_id)
    .fetch_one(pool)
    .await?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::db::size::BaselineKey;
    use crate::db::{DbService, NewGitHubJobset};
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

    async fn jobset(db: &DbService, sha: &str, drv: &str, base: Option<&str>) -> Result<i64> {
        db.create_github_jobset(&NewGitHubJobset {
            sha,
            name: "packages",
            owner: "owner",
            repo_name: "repo",
            jobs: &[hello(drv)],
            config_json: Some(JOB_CONFIG),
            base_sha: base,
            selected: None,
        })
        .await
    }

    fn key() -> BaselineKey<'static> {
        BaselineKey {
            owner: "owner",
            repo_name: "repo",
            branch: "master",
            job: "packages",
            attr_path: "hello",
            system: "x86_64-linux",
        }
    }

    type Result<T> = anyhow::Result<T>;

    #[tokio::test]
    async fn base_drv_is_found_by_attr() -> Result<()> {
        let db = DbService::new_in_memory().await?;
        let base = jobset(&db, "base-sha", BASE_DRV, None).await?;
        let head = jobset(&db, "head-sha", HEAD_DRV, Some("base-sha")).await?;
        let base_drv = DrvId::from_str(BASE_DRV)?;

        let found = get_base_drv_for_attr(head, "hello", &db.pool).await?;
        assert_eq!(found, Some((base, base_drv)));
        assert_eq!(get_base_drv_for_attr(head, "absent", &db.pool).await?, None);
        // A base-branch jobset has no base commit of its own.
        assert_eq!(get_base_drv_for_attr(base, "hello", &db.pool).await?, None);
        Ok(())
    }

    /// The base derivation got its sizes after the base jobset was created
    /// (e.g. measured for another PR) and was never built as part of the
    /// base jobset, so no baseline was recorded. The fallback records it.
    #[tokio::test]
    async fn fallback_records_baseline_from_base_commit() -> Result<()> {
        let db = DbService::new_in_memory().await?;
        let base = jobset(&db, "base-sha", BASE_DRV, None).await?;
        size::set_jobset_branch(&db.pool, base, "master").await?;
        let head = jobset(&db, "head-sha", HEAD_DRV, Some("base-sha")).await?;

        let base_drv = DrvId::from_str(BASE_DRV)?;
        size::update_drv_size(&db.pool, &base_drv, SizeMetric::Output, 1_000).await?;
        size::update_drv_size(&db.pool, &base_drv, SizeMetric::Closure, 9_000).await?;
        assert_eq!(
            size::get_baseline_size(&db.pool, &key(), SizeMetric::Output).await?,
            None
        );

        let checker = SizeChecker::new(db.pool.clone(), None);
        let job_info = JobInfo {
            jobset_id: head,
            name: "hello".to_string(),
            difference: crate::github::JobDifference::Changed,
        };
        checker.ensure_base_commit_baseline(&job_info).await?;

        let closure = size::get_baseline_size(&db.pool, &key(), SizeMetric::Closure).await?;
        assert_eq!(closure, Some(9_000));
        Ok(())
    }
}
