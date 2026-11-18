//! EvalService side of the rebuild strategies: decide which evaluated jobs to
//! build, traverse only those, and hand the jobset to the GitHub service.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use super::{RebuildPlan, RebuildSelection, SelectionInput, repo_roots, select_direct_referrers};
use crate::db::github::BaseJobOutputs;
use crate::github::{CICheckInfo, GitHubTask};
use crate::nix::{EvalJob, EvalService, NixEvalDrv};

/// The base commit is evaluated just before the head, but its jobset is
/// written by the GitHub service; wait up to this long for it to appear.
const BASE_JOBSET_ATTEMPTS: u32 = 6;
const BASE_JOBSET_RETRY: Duration = Duration::from_secs(5);

impl EvalService {
    /// Pick the jobs to build with the `direct-referrers` strategy and
    /// traverse them (or, on fallback, every job) into the dependency graph.
    pub(in crate::nix) async fn plan_and_traverse(
        &self,
        eval_job: &EvalJob,
        ci_info: &CICheckInfo,
        jobs: &[NixEvalDrv],
    ) -> Result<RebuildPlan> {
        let plan = match self.select_jobs(eval_job, ci_info, jobs).await {
            Ok(selection) => {
                info!(
                    "Rebuild strategy direct-referrers for {} at {}: building {} of {} rebuilt \
                     jobs ({} seeds)",
                    eval_job.name,
                    ci_info.commit,
                    selection.selected.len(),
                    selection.rebuilt,
                    selection.seeds.len()
                );
                RebuildPlan::Selective(selection)
            },
            Err(reason) => {
                warn!(
                    "Rebuild strategy direct-referrers for {} at {} builds every job instead: {}",
                    eval_job.name, ci_info.commit, reason
                );
                RebuildPlan::Fallback(reason)
            },
        };

        let to_traverse: Vec<&NixEvalDrv> = match &plan {
            RebuildPlan::Selective(selection) => jobs
                .iter()
                .filter(|job| selection.selected.contains(&job.attr))
                .collect(),
            RebuildPlan::All | RebuildPlan::Fallback(_) => jobs.iter().collect(),
        };
        self.batch_traverse(&to_traverse).await?;
        Ok(plan)
    }

    /// Gather the base jobset and the PR's changed files, then select.
    async fn select_jobs(
        &self,
        eval_job: &EvalJob,
        ci_info: &CICheckInfo,
        jobs: &[NixEvalDrv],
    ) -> Result<RebuildSelection, String> {
        let base_sha = ci_info
            .base_commit
            .as_deref()
            .ok_or("the evaluation has no base commit")?;
        let base = self.base_jobs(base_sha, &eval_job.name).await?;

        let worktree = sandbox::eval::readable_root(Path::new(&eval_job.file_path));
        let changed_files = crate::git::diff_changed_files(&worktree, base_sha, &ci_info.commit)
            .await
            .map_err(|e| format!("could not list the files the PR changes: {e:#}"))?;

        select_direct_referrers(&SelectionInput {
            jobs,
            base: &base,
            changed_files: &changed_files,
            repo_roots: &repo_roots(&worktree),
        })
    }

    /// The base jobset's jobs, waiting briefly for the GitHub service to
    /// record it. Fails when the base jobset is missing or only lists the
    /// jobs that changed against its own base.
    async fn base_jobs(
        &self,
        base_sha: &str,
        job_name: &str,
    ) -> Result<Vec<BaseJobOutputs>, String> {
        let load_err = |e: anyhow::Error| format!("could not load the base jobset: {e:#}");
        for attempt in 1..=BASE_JOBSET_ATTEMPTS {
            let jobs = self
                .db_service
                .get_base_job_outputs(base_sha, job_name)
                .await
                .map_err(load_err)?;
            if !jobs.is_empty() {
                let diffed = self
                    .db_service
                    .get_jobset_base_sha(base_sha, job_name)
                    .await
                    .map_err(load_err)?;
                if diffed.is_some() {
                    return Err(
                        "the base commit was evaluated as a pull request head, so its \
                                jobset lists only the jobs that changed in that pull request"
                            .to_string(),
                    );
                }
                return Ok(jobs);
            }
            if attempt < BASE_JOBSET_ATTEMPTS {
                debug!(
                    "Base jobset {} at {} not recorded yet (attempt {}), retrying",
                    job_name, base_sha, attempt
                );
                tokio::time::sleep(BASE_JOBSET_RETRY).await;
            }
        }
        Err("the base commit has no evaluated jobset for this job".to_string())
    }

    /// Open the eval gate, create the jobset, and complete the gate with the
    /// plan's summary.
    pub(in crate::nix) async fn send_jobset(
        &self,
        ci_info: &Arc<CICheckInfo>,
        name: &str,
        jobs: Vec<NixEvalDrv>,
        config_json: Option<String>,
        plan: RebuildPlan,
    ) -> Result<()> {
        self.open_eval_gate(ci_info, name).await?;
        let summary = plan.summary();
        self.github_sender()?
            .send(GitHubTask::CreateJobSet {
                ci_check_info: Arc::clone(ci_info),
                name: name.to_string(),
                jobs,
                config_json,
                selected_jobs: plan.into_selected(),
            })
            .await?;
        self.complete_eval_gate(ci_info, name, summary).await
    }

    /// Create the `eka-ci / eval / {name}` check run.
    pub(in crate::nix) async fn open_eval_gate(
        &self,
        ci_info: &Arc<CICheckInfo>,
        name: &str,
    ) -> Result<()> {
        self.github_sender()?
            .send(GitHubTask::CreateCIEvalJob {
                ci_check_info: Arc::clone(ci_info),
                job_title: name.to_string(),
            })
            .await?;
        Ok(())
    }

    /// Complete the eval check run successfully; `summary` becomes its output.
    pub(in crate::nix) async fn complete_eval_gate(
        &self,
        ci_info: &Arc<CICheckInfo>,
        name: &str,
        summary: Option<String>,
    ) -> Result<()> {
        self.github_sender()?
            .send(GitHubTask::CompleteCIEvalJob {
                ci_check_info: Arc::clone(ci_info),
                job_name: name.to_string(),
                conclusion: octocrab::params::checks::CheckRunConclusion::Success.into(),
                summary,
            })
            .await?;
        Ok(())
    }

    fn github_sender(&self) -> Result<&tokio::sync::mpsc::Sender<GitHubTask>> {
        self.github_sender.as_ref().context("github sender missing")
    }
}
