//! Creating GitHub jobsets from evaluation results.

use std::collections::HashSet;
use std::str::FromStr;

use tracing::debug;

use super::DbService;
use super::github::{self, BaseJobOutputs};
use super::model::build_event::DrvBuildState;
use super::model::drv::{self, Drv};
use super::model::drv_id::DrvId;
use crate::nix::NixEvalDrv;

/// A jobset to create from an evaluation.
pub struct NewGitHubJobset<'a> {
    pub sha: &'a str,
    pub name: &'a str,
    pub owner: &'a str,
    pub repo_name: &'a str,
    pub jobs: &'a [NixEvalDrv],
    /// Serialized job config, kept for hooks and later lookups
    pub config_json: Option<&'a str>,
    /// Base commit to diff against; `None` for base-commit evaluations
    pub base_sha: Option<&'a str>,
    /// Attrs the job's rebuild strategy chose to build. Every other new or
    /// changed job is recorded as skipped. `None` builds every job.
    pub selected: Option<&'a HashSet<String>>,
}

impl DbService {
    /// Create a jobset and its `Job` rows, diffed against the base jobset.
    pub async fn create_github_jobset(&self, jobset: &NewGitHubJobset<'_>) -> anyhow::Result<i64> {
        let jobset_id = github::create_jobset(
            jobset.sha,
            jobset.name,
            jobset.owner,
            jobset.repo_name,
            jobset.config_json,
            &self.pool,
        )
        .await?;

        // If we have a base commit, query its jobset to compute differences during insertion
        let base_jobset_jobs = match jobset.base_sha {
            Some(base) => {
                github::set_jobset_base_sha(jobset_id, base, &self.pool).await?;
                github::get_jobset_jobs_by_sha(base, jobset.name, &self.pool)
                    .await
                    .ok()
            },
            None => None,
        };

        // Ensure all evaluated drv paths exist in the Drv table before
        // inserting Job rows (which reference Drv via a subquery). Traversed
        // head drvs already exist; base-commit evals and jobs skipped by a
        // rebuild strategy get eval-only rows, which are never loaded into
        // the dependency graph.
        drv::insert_eval_only_drvs(&self.pool, &eval_only_drvs(jobset.jobs)).await?;

        github::create_jobs_for_jobset_with_selection(
            jobset_id,
            jobset.jobs,
            base_jobset_jobs.as_deref(),
            jobset.selected,
            &self.pool,
        )
        .await?;

        // Persist package metadata derived from `iets eval-jobs --meta` onto
        // the `Drv` rows that were created/ensured above.
        drv::update_drv_package_metadata(&self.pool, &metadata_items(jobset.jobs)).await?;

        Ok(jobset_id)
    }

    /// Fetch a drv only if it has been traversed into the dependency graph.
    pub async fn get_traversed_drv(&self, drv_id: &DrvId) -> anyhow::Result<Option<Drv>> {
        drv::get_traversed_drv(drv_id, &self.pool).await
    }

    /// The base jobset's jobs with output hashes and versions.
    pub async fn get_base_job_outputs(
        &self,
        base_sha: &str,
        job_name: &str,
    ) -> anyhow::Result<Vec<BaseJobOutputs>> {
        github::get_base_job_outputs(base_sha, job_name, &self.pool).await
    }

    /// The commit a jobset was diffed against (`None` for full evaluations).
    pub async fn get_jobset_base_sha(
        &self,
        sha: &str,
        job_name: &str,
    ) -> anyhow::Result<Option<String>> {
        github::get_jobset_base_sha(sha, job_name, &self.pool).await
    }
}

/// Placeholder `Drv` rows for evaluated jobs, so `Job` rows can reference
/// them before (or without) traversal.
fn eval_only_drvs(jobs: &[NixEvalDrv]) -> Vec<Drv> {
    jobs.iter()
        .filter_map(|job| {
            let drv_id = DrvId::from_str(&job.drv_path).ok()?;
            Some(Drv {
                drv_path: drv_id,
                system: job.system.clone(),
                prefer_local_build: false,
                required_system_features: None,
                is_fod: false,
                build_state: DrvBuildState::Queued,
                output_size: None,
                closure_size: None,
                pname: job.pname_or_heuristic().into(),
                version: job.version_or_heuristic(),
                license_json: None,
                maintainers_json: None,
                meta_position: None,
                broken: None,
                insecure: None,
            })
        })
        .collect()
}

/// Package metadata per drv. Per-drv DrvId parse failures are logged and
/// dropped (consistent with the existing eval path).
fn metadata_items(jobs: &[NixEvalDrv]) -> Vec<(DrvId, crate::nix::DrvPackageMetadata)> {
    jobs.iter()
        .filter_map(|job| match DrvId::from_str(&job.drv_path) {
            Ok(drv_id) => Some((drv_id, job.to_drv_metadata())),
            Err(e) => {
                debug!(
                    "Skipping metadata persist for un-parseable drv_path {}: {:?}",
                    job.drv_path, e
                );
                None
            },
        })
        .collect()
}

#[cfg(test)]
#[path = "jobset_creation_tests.rs"]
mod tests;
