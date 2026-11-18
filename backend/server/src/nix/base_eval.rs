//! Base-before-head ordering for pull request evaluations.
//!
//! A PR head's jobset is diffed against its base commit's jobset when the
//! head jobset is inserted. If the base jobset does not exist at that point,
//! every head job is classified as new and the whole job set is built. To
//! prevent that, the head eval first makes sure a base jobset exists or has
//! already been dispatched; otherwise it evaluates the base itself and sends
//! its `CreateJobSet` before evaluating the head.
//!
//! Ordering relies on the GitHub service processing tasks FIFO: a base
//! `CreateJobSet` sent before the head's is inserted before the head is
//! diffed.

use std::sync::Arc;

use anyhow::{Context, Result};
use tracing::{info, warn};

use super::{EvalJob, EvalService};
use crate::github::{CICheckInfo, GitHubTask};
use crate::nix::NixEvalDrv;

/// Where to evaluate a PR head's base commit for the same job.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BaseEvalJob {
    /// Job file inside the base commit's worktree.
    pub file_path: String,
    /// Check info of the base commit (`base_commit` is `None`).
    pub ci_info: CICheckInfo,
}

impl EvalService {
    /// Make sure the base jobset of a PR head eval exists, or has been sent
    /// to the GitHub service, before the head is evaluated.
    ///
    /// Returns an error when the base cannot be evaluated, so the head is not
    /// evaluated against a missing base.
    pub(super) async fn ensure_base_jobset(
        &self,
        eval_job: &EvalJob,
        ci_info: &CICheckInfo,
    ) -> Result<()> {
        let Some(base_sha) = ci_info.base_commit.as_deref() else {
            return Ok(());
        };
        if self.base_jobset_known(base_sha, &eval_job.name).await? {
            return Ok(());
        }
        let Some(base) = &eval_job.base else {
            warn!(
                "No jobset for base {} of job {} and no base checkout to evaluate; \
                 all jobs of head {} will be treated as new",
                base_sha, eval_job.name, ci_info.commit
            );
            return Ok(());
        };

        info!(
            "Evaluating base {} of job {} before head {}",
            base_sha, eval_job.name, ci_info.commit
        );
        let (jobs, errors) = self
            .run_eval_jobs(&base.file_path, &[], false)
            .await
            .with_context(|| {
                format!(
                    "base {} of job {} failed to evaluate; not evaluating head {}",
                    base_sha, eval_job.name, ci_info.commit
                )
            })?;
        if !errors.is_empty() {
            warn!(
                "Base {} of job {} evaluated with {} errors; diffing head against the rest",
                base_sha,
                eval_job.name,
                errors.len()
            );
        }
        self.send_base_jobset(&base.ci_info, eval_job, jobs).await
    }

    /// Send the `CreateJobSet` of a base commit and remember it was sent.
    pub(super) async fn send_base_jobset(
        &self,
        base_ci_info: &CICheckInfo,
        eval_job: &EvalJob,
        jobs: Vec<NixEvalDrv>,
    ) -> Result<()> {
        let gh_sender = self
            .github_sender
            .as_ref()
            .context("github sender missing")?;
        gh_sender
            .send(GitHubTask::CreateJobSet {
                ci_check_info: Arc::new(base_ci_info.clone()),
                name: eval_job.name.clone(),
                jobs,
                config_json: eval_job.config_json.clone(),
                selected_jobs: None,
            })
            .await?;
        self.dispatched_base_jobsets
            .lock()
            .await
            .put((base_ci_info.commit.clone(), eval_job.name.clone()), ());
        Ok(())
    }

    /// Whether the base jobset is in the database or was already sent to
    /// the GitHub service (which may not have inserted it yet).
    async fn base_jobset_known(&self, base_sha: &str, job_name: &str) -> Result<bool> {
        let key = (base_sha.to_string(), job_name.to_string());
        if self.dispatched_base_jobsets.lock().await.contains(&key) {
            return Ok(true);
        }
        // Same lookup the head's diff uses (`create_job_set`).
        let existing: Option<i64> =
            sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
                .bind(base_sha)
                .bind(job_name)
                .fetch_optional(&self.db_service.pool)
                .await?;
        Ok(existing.is_some())
    }
}
