pub mod derivation_show;
pub mod jobs;
pub mod nix_eval_jobs;

use std::collections::HashMap;
use std::num::NonZeroUsize;

use anyhow::Result;
use lru::LruCache;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::db::DbService;
use crate::db::model::drv::Drv;
use crate::db::model::drv_id::DrvId;
use crate::db::model::{Reference, Referrer};
use crate::github::{CICheckInfo, GitHubTask};
use crate::scheduler::IngressTask;

pub struct EvalJob {
    pub file_path: String,
    pub name: String,
    pub allow_failures: bool,
    // TODO: support arguments
}

pub enum EvalTask {
    Job(EvalJob),
    GithubJobPR((EvalJob, CICheckInfo)),
    TraverseDrv(String),
}

pub struct EvalService {
    db_service: DbService,
    drv_receiver: mpsc::Receiver<EvalTask>,
    /// Used to request scheduler to determine if it should build a drv
    scheduler_sender: mpsc::Sender<IngressTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    drv_map: LruCache<DrvId, Drv>,
}

impl EvalService {
    pub fn new(
        rcvr: mpsc::Receiver<EvalTask>,
        db_service: DbService,
        scheduler_sender: mpsc::Sender<IngressTask>,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
    ) -> EvalService {
        EvalService {
            db_service,
            drv_receiver: rcvr,
            scheduler_sender,
            github_sender,
            drv_map: LruCache::new(NonZeroUsize::new(5000).unwrap()),
        }
    }

    pub async fn run(mut self, cancellation_token: CancellationToken) {
        while let Some(request) = cancellation_token
            .run_until_cancelled(self.drv_receiver.recv())
            .await
        {
            let task = match request {
                Some(task) => task,
                None => {
                    warn!("Eval receiver channel closed, shutting down");
                    break;
                },
            };

            if let Err(e) = self.handle_eval_task(task).await {
                error!(error = %e, "Failed to handle eval task")
            }
        }

        info!("Eval service shutdown gracefully");
    }

    // TODO: Determine what drvs existed before, to avoid acting like everything is new
    async fn handle_eval_task(&mut self, task: EvalTask) -> Result<()> {
        use anyhow::Context;

        match &task {
            EvalTask::Job(drv) => {
                let (_jobs, _errors) = self.run_nix_eval_jobs(&drv.file_path).await?;
            },
            EvalTask::TraverseDrv(drv) => {
                self.traverse_drvs(drv, &None).await?;
            },
            EvalTask::GithubJobPR((eval_job, ci_info)) => {
                if self.github_sender.is_some() {
                    let (jobs, errors) = self.run_nix_eval_jobs(&eval_job.file_path).await?;
                    let gh_sender = self
                        .github_sender
                        .as_mut()
                        .context("github sender missing")?;

                    // Check if we should fail due to eval errors
                    if !eval_job.allow_failures && !errors.is_empty() {
                        debug!(
                            "Eval job {} has {} errors and allow_failures is false, failing eval \
                             gate",
                            eval_job.name,
                            errors.len()
                        );
                        let fail_task = GitHubTask::FailCIEvalJob {
                            ci_check_info: ci_info.clone(),
                            job_name: eval_job.name.clone(),
                            errors,
                        };
                        gh_sender.send(fail_task).await?;
                        // Don't create jobset or queue builds when eval fails
                        return Ok(());
                    }

                    let create_task = GitHubTask::CreateCIEvalJob {
                        ci_check_info: ci_info.clone(),
                        job_title: eval_job.name.clone(),
                    };
                    gh_sender.send(create_task).await?;

                    let gh_task = GitHubTask::CreateJobSet {
                        ci_check_info: ci_info.clone(),
                        name: eval_job.name.to_string(),
                        jobs,
                    };
                    gh_sender.send(gh_task).await?;

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
    async fn already_visited_drv(&mut self, drv_id: &DrvId) -> bool {
        if self.drv_map.get(drv_id).is_some() {
            return true;
        }

        // If not in cache, check the database
        match self.db_service.get_drv(&drv_id).await {
            Ok(Some(drv)) => {
                // Found in database, add to cache for future lookups
                self.drv_map.put(drv_id.clone(), drv);
                true
            },
            _ => false,
        }
    }

    /// Given a drv, traverse all direct drv dependencies
    async fn traverse_drvs(
        &mut self,
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

    async fn deep_traverse(&mut self, drv_path: &str) -> Result<()> {
        use tokio::task::JoinSet;

        debug!("Traversing drv tree for {}", drv_path);
        let drvs: Vec<DrvId> = drv_requisites(drv_path).await?;
        let new_drvids: Vec<DrvId> = drvs
            .into_iter()
            .filter(|x| self.drv_map.get(x).is_none())
            .collect();
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

        self.db_service
            .insert_drvs_and_references(&new_drvs, &drv_refs)
            .await?;

        for drv in new_drvs {
            let drv_id = drv.drv_path.clone();
            self.scheduler_sender
                .send(IngressTask::EvalRequest(drv_id))
                .await?;
            self.drv_map.put(drv.drv_path.clone(), drv);
        }

        Ok(())
    }
}

/// Retreive the requisites of a drv. This is a global list of all direct
/// and transitive drvs
async fn drv_requisites(drv_path: &str) -> Result<Vec<DrvId>> {
    use std::str::FromStr;

    let output = Command::new("nix-store")
        .args(["--query", "--requisites", drv_path])
        .output()
        .await?
        .stdout;
    let drv_str = String::from_utf8(output)?;

    let drvs = drv_str
        .lines()
        // drv requisites can include "inputSrcs" which are not inputDrvs
        // but rather files which were added to the nix store through
        // path literals or `nix-store --add`
        .filter(|x| x.ends_with(".drv"))
        .map(|x| DrvId::from_str(x).unwrap())
        .collect::<Vec<DrvId>>();

    Ok(drvs)
}

// Retreive the direct dependencies of a drv
pub async fn drv_references(drv_path: &str) -> Result<Vec<String>> {
    let output = Command::new("nix-store")
        .args(["--query", "--references", drv_path])
        .output()
        .await?
        .stdout;
    let drv_str = String::from_utf8(output)?;

    let drvs = drv_str
        .lines()
        // drv references can include "inputSrcs" which are not inputDrvs
        // but rather files which were added to the nix store through
        // path literals or `nix-store --add`
        .filter(|x| x.ends_with(".drv"))
        .map(|x| x.to_string())
        .collect::<Vec<String>>();

    Ok(drvs)
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
