// Re-export evaluator types and functions for backward compatibility
pub use evaluator::nix_utils::{
    self as size, drv_references, drv_requisites, format_size, get_drv_outputs, get_output_sizes,
    is_drv_cached, output_references,
};
pub use evaluator::service::jobs::{
    NIX_EVAL_JOBS_MAX_ENTRIES, NIX_EVAL_JOBS_MAX_LINE_BYTES, NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
    Truncation, process_nix_eval_output,
};
pub use evaluator::types::{
    DrvInfo, DrvOutput, DrvPackageMetadata, NixEvalDrv, NixEvalError, NixEvalItem, NixEvalMeta,
    derivation_show,
};
pub use evaluator::utils::{pname_from_name, version_from_name};

// Server-specific wrapper for eval jobs
mod jobs;

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use lru::LruCache;
use tokio::process::Command;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};

use crate::db::DbService;
use crate::db::model::drv::Drv;
use crate::db::model::drv_id::DrvId;
use crate::db::model::{Reference, Referrer};
use crate::github::{CICheckInfo, GitHubTask};
use crate::graph::GraphCommand;
use crate::metrics::NixEvalMetrics;
use crate::scheduler::IngressTask;

#[derive(Debug)]
pub struct EvalJob {
    pub file_path: String,
    pub name: String,
    pub allow_failures: bool,
    pub config_json: Option<String>, /* Serialized job config for hooks
                                      * TODO: support arguments */
}

#[derive(Debug)]
pub enum EvalTask {
    Job(EvalJob),
    GithubJobPR((EvalJob, CICheckInfo)),
    TraverseDrv(String),
}

pub struct EvalService {
    db_service: DbService,
    eval_sender: mpsc::Sender<EvalTask>,
    eval_receiver: Option<mpsc::Receiver<EvalTask>>,
    /// Used to request scheduler to determine if it should build a drv
    scheduler_sender: mpsc::Sender<IngressTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    graph_command_sender: mpsc::Sender<GraphCommand>,
    drv_map: Mutex<LruCache<DrvId, Drv>>,
    /// M4: metrics for `nix-eval-jobs` output volume and truncation
    /// events. Optional so unit/integration tests that don't care
    /// about observability can pass `None`.
    pub(crate) nix_eval_metrics: Option<Arc<NixEvalMetrics>>,
}

impl EvalService {
    pub fn new(
        sender: mpsc::Sender<EvalTask>,
        receiver: mpsc::Receiver<EvalTask>,
        db_service: DbService,
        scheduler_sender: mpsc::Sender<IngressTask>,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        graph_command_sender: mpsc::Sender<GraphCommand>,
        nix_eval_metrics: Option<Arc<NixEvalMetrics>>,
    ) -> EvalService {
        EvalService {
            db_service,
            eval_sender: sender,
            eval_receiver: Some(receiver),
            scheduler_sender,
            github_sender,
            graph_command_sender,
            drv_map: Mutex::new(LruCache::new(NonZeroUsize::new(5000).unwrap())),
            nix_eval_metrics,
        }
    }

    async fn handle_eval_task(&self, task: EvalTask) -> Result<()> {
        use anyhow::Context;

        debug!("EvalService received task: {:?}", task);

        match &task {
            EvalTask::Job(drv) => {
                debug!("Processing Job task for: {}", drv.file_path);
                let (_jobs, _errors) = self.run_nix_eval_jobs(&drv.file_path, true).await?;
            },
            EvalTask::TraverseDrv(drv) => {
                debug!("Processing TraverseDrv task for: {}", drv);
                self.traverse_drvs(drv, &None).await?;
            },
            EvalTask::GithubJobPR((eval_job, ci_info)) => {
                if self.github_sender.is_some() {
                    // Only traverse drvs for head commits (base_commit is
                    // Some). Base-commit evals only need the attr/drv list
                    // for jobset diff computation — no graph population or
                    // build scheduling required.
                    let is_head = ci_info.base_commit.is_some();
                    let (jobs, errors) =
                        self.run_nix_eval_jobs(&eval_job.file_path, is_head).await?;
                    let gh_sender = self
                        .github_sender
                        .as_ref()
                        .context("github sender missing")?;
                    // Clone once into Arc so the 2–3 downstream sends share one refcount.
                    let ci_info = std::sync::Arc::new((*ci_info).clone());

                    // Check if we should fail due to eval errors
                    if !eval_job.allow_failures && !errors.is_empty() {
                        debug!(
                            "Eval job {} has {} errors and allow_failures is false, failing eval \
                             gate",
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
                        ci_check_info: ci_info,
                        name: eval_job.name.to_string(),
                        jobs: jobs.clone(),
                        config_json: eval_job.config_json.clone(),
                    };
                    gh_sender.send(gh_task).await?;

                    // Queue each evaluated drv for build scheduling.
                    // Only head commits (with base_commit) need builds;
                    // base-commit evals only populate the jobset for diffs.
                    if is_head {
                        for job in &jobs {
                            if let Ok(drv_id) = std::str::FromStr::from_str(&job.drv_path) {
                                self.scheduler_sender
                                    .send(IngressTask::EvalRequest(std::sync::Arc::new(drv_id)))
                                    .await?;
                            }
                        }
                    }

                    // The eval gate will remain InProgress until all jobs are concluded
                    // It will be completed by the recorder when the last job finishes
                } else {
                    warn!("GitHub service was never initialized, skipping task to create a jobset")
                }
            },
        };

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

    async fn deep_traverse(&self, drv_path: &str) -> Result<()> {
        use tokio::task::JoinSet;

        debug!("Traversing drv tree for {}", drv_path);
        let drvs: Vec<DrvId> = drv_requisites_as_ids(drv_path).await?;
        let mut drv_map = self.drv_map.lock().await;
        let new_drvids: Vec<DrvId> = drvs
            .into_iter()
            .filter(|x| drv_map.get(x).is_none())
            .collect();
        drop(drv_map); // Release the lock before async operations
        debug!("Found {} new drvs", new_drvids.len());

        let mut new_drvs = Vec::new();
        let mut drv_refs: Vec<(DrvId, DrvId)> = Vec::new();

        for drvs_chunk in new_drvids.chunks(150) {
            let mut info_set: JoinSet<Result<Drv, anyhow::Error>> = JoinSet::new();
            let mut ref_set: JoinSet<Result<Vec<(Referrer, Reference)>, anyhow::Error>> =
                JoinSet::new();

            for drv in drvs_chunk {
                let drv_to_fetch = drv.store_path();
                let db_service = self.db_service.clone();
                info_set.spawn(async move { Drv::fetch_info(&drv_to_fetch, &db_service).await });
                let drv_clone = drv.clone();
                ref_set.spawn(async move { drv_clone.reference_pairs().await });
            }
            let fetched_drvs = info_set.join_all().await;
            let new_drv_refs = ref_set.join_all().await;

            let successful_fetches = fetched_drvs.into_iter().collect::<Result<Vec<_>>>()?;
            let successful_refs = new_drv_refs
                .into_iter()
                .flat_map(|x| x.into_iter().flatten())
                .collect::<Vec<(DrvId, DrvId)>>();

            new_drvs.extend(successful_fetches);
            drv_refs.extend(successful_refs);
        }

        // Insert into database for persistence
        self.db_service
            .insert_drvs_and_references(&new_drvs, &drv_refs)
            .await?;

        // Insert into graph for fast in-memory access.
        // Fire-and-forget: awaiting the oneshot response here
        // deadlocks when the graph channel is saturated by
        // concurrent IngressTask::EvalRequest processing from
        // earlier traversals.
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let cmd = GraphCommand::InsertDrvs {
            drvs: crate::graph_compat::to_shared_drvs(&new_drvs)?,
            refs: drv_refs
                .clone()
                .into_iter()
                .map(|(r, d)| {
                    Ok((
                        crate::graph_compat::to_shared_drv_id(&r)?,
                        crate::graph_compat::to_shared_drv_id(&d)?,
                    ))
                })
                .collect::<anyhow::Result<Vec<_>>>()?,
            response: tx,
        };
        self.graph_command_sender.send(cmd).await?;

        // NOTE: ingress EvalRequests are NOT sent from deep_traverse.
        // Sending 12k+ EvalRequests here saturates the ingress channel
        // and deadlocks the eval service (ingress → recorder → github
        // back-pressure prevents the channel from draining while the
        // eval service is blocked trying to send more). Instead, the
        // GithubJobPR handler sends EvalRequests after the jobset is
        // created via CreateJobSet, which is the correct place since
        // only drvs with job entries need ingress processing.

        // Then acquire lock once and batch update the cache
        let mut drv_map = self.drv_map.lock().await;
        for drv in new_drvs {
            drv_map.put(drv.drv_path.clone(), drv);
        }
        drop(drv_map); // Explicit unlock

        Ok(())
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

/// Result of a `nix-store --realise --dry-run` invocation against a derivation.
///
/// Nix prints two relevant sections to stderr:
/// * `(this|these N) derivation(s) will be built:` followed by indented `.drv` paths that are NOT
///   available locally and CANNOT be substituted from any configured binary cache — i.e. they would
///   actually need to run.
/// * `(this|these N) path(s) will be fetched (...)` followed by indented store output paths that
///   are not in the local store but ARE available from a binary cache.
///
/// Anything not mentioned in either section is already valid in the local store.
///
/// We treat a derivation as "cached" iff its `.drv` path does not appear in
/// `will_build` — meaning it is either already built locally OR pullable from
/// a substituter, both of which mean we don't need to run a real build.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DryRunReport {
    /// Full store paths (`/nix/store/...drv`) that nix says still need building.
    pub will_build: HashSet<String>,
    /// Full store paths (`/nix/store/...`) that nix says will be substituted.
    pub will_fetch: HashSet<String>,
}

impl DryRunReport {
    /// Parse the stderr emitted by `nix-store --realise --dry-run`.
    ///
    /// The format is fairly stable across modern Nix versions; we tolerate
    /// minor variations (singular vs. plural section headers, optional
    /// download-size annotations, leading whitespace differences).
    pub fn parse(stderr: &str) -> Self {
        #[derive(Clone, Copy)]
        enum Section {
            Build,
            Fetch,
        }

        let mut report = DryRunReport::default();
        let mut current: Option<Section> = None;

        for raw_line in stderr.lines() {
            // Identify section headers regardless of pluralization. Nix emits
            // these without leading whitespace and ending with `:`.
            let trimmed = raw_line.trim_end();
            let lower = trimmed.trim_start().to_ascii_lowercase();

            // Section headers start a new section. Order matters: check
            // headers BEFORE treating an indented line as a path, because
            // a header is never indented in practice but we trim defensively.
            if !raw_line.starts_with(char::is_whitespace) {
                if lower.contains("will be built") || lower.starts_with("don't know how to build") {
                    current = Some(Section::Build);
                    continue;
                }
                if lower.contains("will be fetched") {
                    current = Some(Section::Fetch);
                    continue;
                }
                // Any other unindented non-blank line ends the current section
                // (e.g. warnings, the empty line nix prints between sections,
                // the final summary line).
                if !trimmed.is_empty() {
                    current = None;
                }
                continue;
            }

            // Indented line — interpret as a path entry IF we're in a section.
            let path = trimmed.trim();
            if path.is_empty() {
                continue;
            }
            // Only accept absolute store paths; ignore stray indented text.
            if !path.starts_with('/') {
                continue;
            }
            match current {
                Some(Section::Build) => {
                    report.will_build.insert(path.to_string());
                },
                Some(Section::Fetch) => {
                    report.will_fetch.insert(path.to_string());
                },
                None => {},
            }
        }

        report
    }

    /// True iff `drv` does not appear in `will_build` — i.e. its output is
    /// either already in the local store or pullable from a substituter.
    pub fn is_cached(&self, drv: &DrvId) -> bool {
        !self.will_build.contains(&drv.store_path())
    }
}

/// How long to wait for `nix-store --realise --dry-run` to complete before
/// giving up. Substituter HTTP queries are the dominant cost; 30s comfortably
/// covers a slow but reachable cache while still failing fast on a wedged one.
const DRY_RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Run `nix-store --realise --dry-run <drv>` and parse the result.
///
/// Returns `Err` for any I/O / process failure or non-zero exit so callers can
/// safely fall back to a real build (substitution checks are an optimization,
/// never a correctness requirement).
pub async fn dry_run_realise(drv: &DrvId) -> Result<DryRunReport> {
    let store_path = drv.store_path();
    let output = tokio::time::timeout(
        DRY_RUN_TIMEOUT,
        Command::new("nix-store")
            .args(["--realise", "--dry-run", &store_path])
            .output(),
    )
    .await
    .with_context(|| format!("nix-store --realise --dry-run timed out for {store_path}"))?
    .with_context(|| format!("failed to spawn nix-store --realise --dry-run for {store_path}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "nix-store --realise --dry-run failed for {} (exit {:?}): {}",
            store_path,
            output.status.code(),
            stderr.trim()
        );
    }

    // Nix writes the build/fetch plan to stderr; stdout is empty under --dry-run.
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(DryRunReport::parse(&stderr))
}

// The following functions are now imported from evaluator and re-exported above:
// - drv_requisites: Returns Vec<String> of drv paths
// - drv_references: Returns Vec<String> of direct drv dependencies
// - output_references: Returns Vec<String> of runtime references for an output
// - get_drv_outputs: Returns HashMap<String, String> of output names to paths

// Helper wrapper to convert drv_requisites result to Vec<DrvId> for server use
pub(crate) async fn drv_requisites_as_ids(drv_path: &str) -> Result<Vec<DrvId>> {
    use std::str::FromStr;
    let drvs = drv_requisites(drv_path).await?;
    drvs.into_iter()
        .map(|s| DrvId::from_str(&s))
        .collect::<Result<Vec<DrvId>, _>>()
        .context("failed to parse drv path from requisites output")
}

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
        assert!(report.is_cached(&drv("hello")));
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
        assert!(!report.is_cached(&target));

        // The drv is only in will_fetch (its OUTPUT is fetchable from cache).
        // The .drv path itself is not in will_build, so we're "cached".
        let report = DryRunReport::parse(
            "these 1 paths will be fetched (1 KiB download):\n  /nix/store/aaa-foo\n",
        );
        assert!(report.is_cached(&target));
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
