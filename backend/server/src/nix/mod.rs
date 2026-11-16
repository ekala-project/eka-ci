// Re-export evaluator types and functions for backward compatibility
pub use evaluator::nix_utils::{
    self as size, DryRunReport, drv_references, drv_requisites, drv_requisites_many, format_size,
    get_drv_outputs, is_drv_cached, output_references,
};
pub use evaluator::service::jobs::{
    EVAL_JOBS_MAX_ENTRIES, EVAL_JOBS_MAX_LINE_BYTES, EVAL_JOBS_MAX_STDOUT_BYTES, Truncation,
    process_eval_output,
};
pub use evaluator::types::{
    DrvInfo, DrvOutput, DrvPackageMetadata, NixEvalDrv, NixEvalError, NixEvalItem, NixEvalMeta,
    derivation_show,
};
pub use evaluator::utils::{pname_from_name, version_from_name};

// Server-specific wrapper for eval jobs
mod jobs;
mod traverse;

pub mod reconstitute;

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use anyhow::Result;
use lru::LruCache;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};

use crate::db::DbService;
use crate::db::model::drv::Drv;
use crate::db::model::drv_id::DrvId;
use crate::github::{CICheckInfo, GitHubTask};
use crate::graph::GraphCommand;
use crate::metrics::NixEvalMetrics;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct EvalJob {
    pub file_path: String,
    pub name: String,
    pub allow_failures: bool,
    pub config_json: Option<String>, /* Serialized job config for hooks
                                      * TODO: support arguments */
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum EvalTask {
    Job(EvalJob),
    GithubJobPR((EvalJob, CICheckInfo)),
    TraverseDrv(String),
    /// Evaluate `passthru.tests` for directly-changed packages.
    ///
    /// Triggered by the GitHub service after jobset diff identifies
    /// which packages were directly modified (filtered via
    /// `meta.position` + `git diff`).
    PassthruTests {
        file_path: String,
        changed_attrs: Vec<String>,
        ci_info: CICheckInfo,
        parent_job_name: String,
        config_json: Option<String>,
    },
}

pub struct EvalService {
    db_service: DbService,
    eval_sender: mpsc::Sender<EvalTask>,
    eval_receiver: Option<mpsc::Receiver<EvalTask>>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    graph_command_sender: mpsc::Sender<GraphCommand>,
    drv_map: Mutex<LruCache<DrvId, Drv>>,
    /// M4: metrics for `iets eval-jobs` output volume and truncation
    /// events. Optional so unit/integration tests that don't care
    /// about observability can pass `None`.
    pub(crate) nix_eval_metrics: Option<Arc<NixEvalMetrics>>,
    eval_sandbox: sandbox::eval::EvalSandbox,
    journal: crate::services::TaskJournal<EvalTask>,
}

impl EvalService {
    pub fn new(
        sender: mpsc::Sender<EvalTask>,
        receiver: mpsc::Receiver<EvalTask>,
        db_service: DbService,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        graph_command_sender: mpsc::Sender<GraphCommand>,
        nix_eval_metrics: Option<Arc<NixEvalMetrics>>,
        eval_sandbox: sandbox::eval::EvalSandbox,
    ) -> EvalService {
        let pool = db_service.pool.clone();
        EvalService {
            db_service,
            eval_sender: sender,
            eval_receiver: Some(receiver),
            github_sender,
            graph_command_sender,
            drv_map: Mutex::new(LruCache::new(NonZeroUsize::new(5000).unwrap())),
            nix_eval_metrics,
            eval_sandbox,
            journal: crate::services::TaskJournal::new(pool, "eval"),
        }
    }

    async fn handle_eval_task(&self, task: EvalTask) -> Result<()> {
        debug!("EvalService received task: {:?}", task);

        match &task {
            EvalTask::Job(drv) => {
                debug!("Processing Job task for: {}", drv.file_path);
                let (_jobs, _errors) = self.run_eval_jobs(&drv.file_path, &[], true).await?;
            },
            EvalTask::TraverseDrv(drv) => {
                debug!("Processing TraverseDrv task for: {}", drv);
                self.traverse_drvs(drv, &None).await?;
            },
            EvalTask::GithubJobPR((eval_job, ci_info)) => {
                if self.github_sender.is_some() {
                    self.handle_github_job_pr(eval_job, ci_info).await?;
                } else {
                    warn!("GitHub service was never initialized, skipping task to create a jobset")
                }
            },
            EvalTask::PassthruTests {
                file_path,
                changed_attrs,
                ci_info,
                parent_job_name,
                config_json,
            } => {
                if self.github_sender.is_some() {
                    self.handle_passthru_tests(
                        file_path,
                        changed_attrs,
                        ci_info,
                        parent_job_name,
                        config_json.as_deref(),
                    )
                    .await?;
                } else {
                    warn!(
                        "GitHub service was never initialized, skipping passthru.tests evaluation"
                    )
                }
            },
        };

        Ok(())
    }

    async fn handle_github_job_pr(&self, eval_job: &EvalJob, ci_info: &CICheckInfo) -> Result<()> {
        use anyhow::Context;

        // Only traverse drvs for head commits (base_commit is
        // Some). Base-commit evals only need the attr/drv list
        // for jobset diff computation — no graph population or
        // build scheduling required.
        let is_head = ci_info.base_commit.is_some();
        let (jobs, errors) = self
            .run_eval_jobs(&eval_job.file_path, &[], is_head)
            .await?;
        let gh_sender = self
            .github_sender
            .as_ref()
            .context("github sender missing")?;
        // Clone once into Arc so the 2–3 downstream sends share one refcount.
        let ci_info = std::sync::Arc::new(ci_info.clone());

        // Check if we should fail due to eval errors
        if !eval_job.allow_failures && !errors.is_empty() {
            debug!(
                "Eval job {} has {} errors and allow_failures is false, failing eval gate",
                eval_job.name,
                errors.len()
            );
            let fail_task = GitHubTask::FailCIEvalJob {
                ci_check_info: ci_info,
                job_name: eval_job.name.clone(),
                errors,
            };
            gh_sender.send(fail_task).await?;
            // Don't create jobset or queue builds when eval fails
            return Ok(());
        }

        let create_task = GitHubTask::CreateCIEvalJob {
            ci_check_info: std::sync::Arc::clone(&ci_info),
            job_title: eval_job.name.clone(),
        };
        gh_sender.send(create_task).await?;

        let gh_task = GitHubTask::CreateJobSet {
            ci_check_info: std::sync::Arc::clone(&ci_info),
            name: eval_job.name.to_string(),
            jobs,
            config_json: eval_job.config_json.clone(),
        };
        gh_sender.send(gh_task).await?;

        // Complete the eval gate immediately — evaluation succeeded
        // and all per-package check runs were emitted.
        let complete_task = GitHubTask::CompleteCIEvalJob {
            ci_check_info: ci_info,
            job_name: eval_job.name.clone(),
            conclusion: octocrab::params::checks::CheckRunConclusion::Success.into(),
        };
        gh_sender.send(complete_task).await?;

        Ok(())
    }

    async fn handle_passthru_tests(
        &self,
        file_path: &str,
        changed_attrs: &[String],
        ci_info: &CICheckInfo,
        parent_job_name: &str,
        config_json: Option<&str>,
    ) -> Result<()> {
        if changed_attrs.is_empty() {
            debug!("No changed attrs for passthru.tests, skipping");
            return Ok(());
        }

        let job_name = format!("{}/passthru-tests", parent_job_name);
        info!(
            "Evaluating passthru.tests for {} changed attrs in job {}",
            changed_attrs.len(),
            job_name
        );

        let jobs = self
            .eval_passthru_tests_expr(file_path, changed_attrs)
            .await?;
        if jobs.is_empty() {
            debug!("No passthru.tests derivations found, skipping jobset creation");
            return Ok(());
        }

        info!(
            "Found {} passthru.tests derivations for job {}",
            jobs.len(),
            job_name
        );
        self.send_passthru_tests_jobset(&job_name, jobs, ci_info, config_json)
            .await
    }

    /// Generate and evaluate the passthru.tests Nix expression.
    async fn eval_passthru_tests_expr(
        &self,
        file_path: &str,
        changed_attrs: &[String],
    ) -> Result<Vec<NixEvalDrv>> {
        use anyhow::Context;

        let tmp_file =
            evaluator::passthru_tests::generate_passthru_tests_expr(file_path, changed_attrs)?;
        let tmp_path = tmp_file
            .path()
            .to_str()
            .context("temp file path is not valid UTF-8")?;

        let original_root = sandbox::eval::readable_root(std::path::Path::new(file_path));
        let (jobs, _errors) = self.run_eval_jobs(tmp_path, &[original_root], true).await?;
        drop(tmp_file);
        Ok(jobs)
    }

    /// Send the passthru.tests results as a new jobset to the GitHub service.
    async fn send_passthru_tests_jobset(
        &self,
        job_name: &str,
        jobs: Vec<NixEvalDrv>,
        ci_info: &CICheckInfo,
        config_json: Option<&str>,
    ) -> Result<()> {
        use anyhow::Context;

        let gh_sender = self
            .github_sender
            .as_ref()
            .context("github sender missing")?;
        let ci_info = std::sync::Arc::new(ci_info.clone());

        let create_task = GitHubTask::CreateCIEvalJob {
            ci_check_info: std::sync::Arc::clone(&ci_info),
            job_title: job_name.to_string(),
        };
        gh_sender.send(create_task).await?;

        let gh_task = GitHubTask::CreateJobSet {
            ci_check_info: std::sync::Arc::clone(&ci_info),
            name: job_name.to_string(),
            jobs,
            config_json: config_json.map(str::to_string),
        };
        gh_sender.send(gh_task).await?;

        let complete_task = GitHubTask::CompleteCIEvalJob {
            ci_check_info: ci_info,
            job_name: job_name.to_string(),
            conclusion: octocrab::params::checks::CheckRunConclusion::Success.into(),
        };
        gh_sender.send(complete_task).await?;

        Ok(())
    }

    /// check the drv_map if it contains the drv_id, then check the database
    /// if it's just not in the LRU cache.
    async fn already_visited_drv(&self, drv_id: &DrvId) -> bool {
        // First check cache with explicit scoping to ensure lock is released
        {
            let mut drv_map = self.drv_map.lock().await;
            if drv_map.get(drv_id).is_some() {
                return true;
            }
        } // Lock explicitly dropped here

        // If not in cache, check the database (no lock held)
        match self.db_service.get_drv(drv_id).await {
            Ok(Some(drv)) => {
                // Found in database, add to cache for future lookups
                self.drv_map.lock().await.put(drv_id.clone(), drv);
                true
            },
            _ => false,
        }
    }

    /// Given a drv, traverse all direct drv dependencies
    async fn traverse_drvs(
        &self,
        drv_path: &str,
        _references: &Option<HashMap<String, Vec<String>>>,
    ) -> Result<()> {
        use std::str::FromStr;

        let drv_id = DrvId::from_str(drv_path)?;
        if self.already_visited_drv(&drv_id).await {
            return Ok(());
        }

        // TODO: see if we can leverage reference information
        // For now, deeply traversing everything ensures we capture all drv
        // dependencies
        self.deep_traverse(drv_path).await

        // let mut drv_pairs = Vec::new();
        // match references {
        //     None => self.deep_traverse(drv_path).await?,
        //     Some(reference_map) => {
        //         for reference in reference_map.keys() {
        //             Box::pin(self.traverse_drvs(&reference, &None)).await?;
        //             let reference_id = DrvId::from_str(drv_path)?;
        //             drv_pairs.push((reference_id, drv_id.clone()));
        //         }
        //     },
        // }

        // let drv = Drv::fetch_info(drv_path, &self.db_service).await?;
        // let drv_slice = &[drv.clone()];
        // self.db_service
        //     .insert_drvs_and_references(&drv_slice[..], &drv_pairs)
        //     .await?;

        // self.scheduler_sender
        //     .send(IngressTask::EvalRequest(drv_id))
        //     .await?;
        // self.drv_map.put(drv.drv_path.clone(), drv);

        // Ok(())
    }
}

impl crate::services::AsyncService<EvalTask> for EvalService {
    fn get_sender(&self) -> mpsc::Sender<EvalTask> {
        self.eval_sender.clone()
    }

    #[allow(dead_code)] // Called via AsyncService trait dispatch
    fn take_receiver(&mut self) -> Option<mpsc::Receiver<EvalTask>> {
        self.eval_receiver.take()
    }

    fn task_journal(&self) -> Option<&crate::services::TaskJournal<EvalTask>> {
        Some(&self.journal)
    }

    async fn handle_task(&self, task: EvalTask) -> Result<()> {
        self.handle_eval_task(task).await
    }

    async fn handle_failure(&mut self, error: anyhow::Error) {
        error!("EvalService task failed: {:?}", error);
    }

    async fn handle_closure(&mut self) {
        info!("EvalService shutting down");
    }
}

/// Server-side wrapper: delegates to evaluator's `dry_run_realise` with `DrvId` → store path.
pub async fn dry_run_realise(drv: &DrvId) -> Result<DryRunReport> {
    evaluator::nix_utils::dry_run_realise(&drv.store_path()).await
}

// The following functions are now imported from evaluator and re-exported above:
// - drv_requisites / drv_requisites_many: Returns Vec<String> of drv paths
// - drv_references: Returns Vec<String> of direct drv dependencies
// - output_references: Returns Vec<String> of runtime references for an output
// - get_drv_outputs: Returns HashMap<String, String> of output names to paths

// fn graph_line_to_drvids(drv_line: &str) -> Result<(DrvId, DrvId)> {
//     let mut line = drv_line.split(" ");
//     let reference: DrvId = graph_str_to_drvid(line.next().unwrap())?;
//     // drop inner "->"
//     line.next().unwrap();
//     let referrer = graph_str_to_drvid(line.next().unwrap())?;
//
//     Ok((reference, referrer))
// }
//
// /// This assumes a well-formated string from the output of nix-store --query --graph
// fn graph_str_to_drvid(drv_str: &str) -> Result<DrvId> {
//     use std::str::FromStr;
//
//     use anyhow::bail;
//
//     let mut reference_string: String = drv_str.to_string();
//     reference_string.retain(|c| c != '"');
//     if !reference_string.ends_with(".drv") {
//         bail!("not a drv");
//     }
//     DrvId::from_str(&reference_string)
// }
//
// /// Retreive the entirity of a drv's reference graph.
// /// This uses `nix-store --query --graph` to construct
// /// the whole graph in one invocation
// /// Returns: Vec<(reference, referrer)>, where the referrer consumes (downstream of) a reference
// fn drv_reference_graph(drv_path: &str) -> Result<Vec<(DrvId, DrvId)>> {
//     let output = Command::new("nix-store")
//         .args(["--query", "--graph", drv_path])
//         .output()?
//         .stdout;
//     let drv_str = String::from_utf8(output)?;
//
//     let drvs = drv_str
//         .lines()
//         // The graph includes inputSrcs as well as graphviz node information
//         // Filtering by " -> " assures we are only grabbing edges
//         .filter(|x| x.contains(" -> "))
//         .filter_map(|x| graph_line_to_drvids(x).ok())
//         .filter(| (x,y) | x != y)
//         .collect::<Vec<(_, _)>>();
//
//     debug!("drv_graph: {:?}", drvs);
//
//     Ok(drvs)
// }

#[cfg(test)]
mod dry_run_tests {
    use std::str::FromStr;

    use super::*;

    fn drv(name: &str) -> DrvId {
        // 32-char base32-only stub hash; the actual value is irrelevant — we
        // only need a syntactically valid DrvId so we can call store_path().
        DrvId::from_str(&format!("jd83l3jn2mkn530lgcg0y523jq5qji85-{name}.drv")).unwrap()
    }

    #[test]
    fn parse_empty_means_already_built_locally() {
        let report = DryRunReport::parse("");
        assert!(report.will_build.is_empty());
        assert!(report.will_fetch.is_empty());
        assert!(report.is_cached_path(&drv("hello").store_path()));
    }

    #[test]
    fn parse_will_be_built_singular() {
        let stderr = "this derivation will be built:\n  /nix/store/aaa-foo.drv\n";
        let report = DryRunReport::parse(stderr);
        assert_eq!(report.will_build.len(), 1);
        assert!(report.will_build.contains("/nix/store/aaa-foo.drv"));
        assert!(report.will_fetch.is_empty());
    }

    #[test]
    fn parse_will_be_built_plural_with_count() {
        let stderr = "these 3 derivations will be built:\n  /nix/store/a-foo.drv\n  \
                      /nix/store/b-bar.drv\n  /nix/store/c-baz.drv\n";
        let report = DryRunReport::parse(stderr);
        assert_eq!(report.will_build.len(), 3);
    }

    #[test]
    fn parse_will_be_fetched_with_size_annotation() {
        let stderr = "these 2 paths will be fetched (1.23 MiB download, 5.67 MiB unpacked):\n  \
                      /nix/store/aaa-foo\n  /nix/store/bbb-bar\n";
        let report = DryRunReport::parse(stderr);
        assert!(report.will_build.is_empty());
        assert_eq!(report.will_fetch.len(), 2);
        assert!(report.will_fetch.contains("/nix/store/aaa-foo"));
        assert!(report.will_fetch.contains("/nix/store/bbb-bar"));
    }

    #[test]
    fn parse_mixed_sections() {
        let stderr = "these 2 derivations will be built:\n  /nix/store/a-build.drv\n  \
                      /nix/store/b-build.drv\nthese 1 paths will be fetched (1 KiB download):\n  \
                      /nix/store/c-fetch\n";
        let report = DryRunReport::parse(stderr);
        assert_eq!(report.will_build.len(), 2);
        assert_eq!(report.will_fetch.len(), 1);
    }

    #[test]
    fn is_cached_uses_will_build_membership() {
        // The drv we are checking is in will_build — NOT cached.
        let target = drv("foo");
        let stderr = format!(
            "this derivation will be built:\n  {}\n",
            target.store_path()
        );
        let report = DryRunReport::parse(&stderr);
        assert!(!report.is_cached_path(&target.store_path()));

        // The drv is only in will_fetch (its OUTPUT is fetchable from cache).
        // The .drv path itself is not in will_build, so we're "cached".
        let report = DryRunReport::parse(
            "these 1 paths will be fetched (1 KiB download):\n  /nix/store/aaa-foo\n",
        );
        assert!(report.is_cached_path(&target.store_path()));
    }

    #[test]
    fn parse_dont_know_how_to_build_treated_as_build() {
        // When substitution is unavailable and the drv isn't in the local store,
        // nix prints "don't know how to build the following paths:" — we treat
        // these the same as "will be built" so the caller falls back to the
        // normal build path (which will surface the error properly).
        let stderr = "don't know how to build the following paths:\n  /nix/store/x-foo.drv\n";
        let report = DryRunReport::parse(stderr);
        assert_eq!(report.will_build.len(), 1);
        assert!(report.will_build.contains("/nix/store/x-foo.drv"));
    }

    #[test]
    fn parse_ignores_non_path_indented_lines() {
        // An indented line that isn't an absolute store path must not pollute
        // the section — guards against future nix output additions like
        // "  (note: ...)" inside a section.
        let stderr = "this derivation will be built:\n  /nix/store/aaa-foo.drv\n  (note: foo)\n";
        let report = DryRunReport::parse(stderr);
        assert_eq!(report.will_build.len(), 1);
    }

    #[test]
    fn parse_section_resets_on_unrelated_unindented_line() {
        // Unindented non-blank lines that aren't section headers terminate the
        // active section so warnings don't get mis-attributed as path entries.
        let stderr = "this derivation will be built:\n  /nix/store/aaa-foo.drv\nwarning: \
                      something\n  /nix/store/bbb-bar\n";
        let report = DryRunReport::parse(stderr);
        assert_eq!(report.will_build.len(), 1);
        assert!(report.will_build.contains("/nix/store/aaa-foo.drv"));
        // The bbb-bar line came after the section was reset, so it's neither.
        assert!(report.will_fetch.is_empty());
    }

    #[test]
    fn parse_blank_line_between_sections() {
        let stderr = "these 1 derivations will be built:\n  /nix/store/a-foo.drv\n\nthese 1 paths \
                      will be fetched (1 KiB):\n  /nix/store/b-bar\n";
        let report = DryRunReport::parse(stderr);
        assert_eq!(report.will_build.len(), 1);
        assert_eq!(report.will_fetch.len(), 1);
    }
}
