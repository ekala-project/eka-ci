pub mod derivation_show;
pub mod jobs;
pub mod nix_eval_jobs;

use std::collections::HashMap;
use std::process::Command;

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::db::DbService;
use crate::db::model::drv::Drv;
use crate::db::model::drv_id::DrvId;
use crate::scheduler::IngressTask;

pub struct EvalJob {
    pub file_path: String,
    // TODO: support arguments
}

pub enum EvalTask {
    Job(EvalJob),
    TraverseDrv(String),
}

pub struct EvalService {
    db_service: DbService,
    drv_receiver: mpsc::Receiver<EvalTask>,
    /// Used to request scheduler to determine if it should build a drv
    scheduler_sender: mpsc::Sender<IngressTask>,
    // TODO: Eventually this should be an LRU cache
    // This allows for us to memoize visited drvs so we don't have to revisit
    // common drvs (e.g. stdenv)
    drv_map: HashMap<DrvId, Drv>,
}

impl EvalService {
    pub fn new(
        rcvr: mpsc::Receiver<EvalTask>,
        db_service: DbService,
        scheduler_sender: mpsc::Sender<IngressTask>,
    ) -> EvalService {
        EvalService {
            db_service,
            drv_receiver: rcvr,
            scheduler_sender,
            drv_map: HashMap::new(),
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

            let result = match &task {
                EvalTask::Job(drv) => self.run_nix_eval_jobs(&drv.file_path).await,
                EvalTask::TraverseDrv(drv) => self.traverse_drvs(&drv).await,
            };

            if let Err(e) = result {
                error!(error = %e, "Failed to handle task")
            }
        }

        info!("Eval service shutdown gracefully");
    }

    /// Given a drv, traverse all direct drv dependencies
    async fn traverse_drvs(&mut self, drv_path: &str) -> Result<()> {
        use tokio::task::JoinSet;

        debug!("Entering traverse drvs");
        let drvs: Vec<DrvId> = drv_requisites(drv_path)?;
        // Check if this drv has been visited in a previous evaluation.
        let new_drvids: Vec<DrvId> = drvs
            .into_iter()
            .filter(|x| !self.drv_map.contains_key(x))
            .collect();

        // resolve drv info in parallel
        // If there's over ~400, we quickly exhaust file handles, so take a slower path
        let mut new_drvs = Vec::new();
        for drvs_chunk in new_drvids.chunks(300) {
            let mut info_set: JoinSet<Result<Drv, anyhow::Error>> = JoinSet::new();

            for drv in drvs_chunk {
                let drv_to_fetch = drv.store_path();
                info_set.spawn(async move { Drv::fetch_info(&drv_to_fetch).await });
            }
            let fetched_drvs = info_set.join_all().await;
            let successful_fetches = fetched_drvs
                .into_iter()
                .collect::<anyhow::Result<Vec<_>>>()?;

            new_drvs.extend(successful_fetches);
        }

        let drv_refs: Vec<(DrvId, DrvId)> = drv_reference_graph(drv_path)?;

        self.db_service
            .insert_drvs_and_references(&new_drvs, &drv_refs)
            .await?;

        for drv in new_drvids {
            self.scheduler_sender
                .send(IngressTask::EvalRequest(drv))
                .await?;
        }

        Ok(())
    }
}

/// Retreive the requisites of a drv. This is a global list of all direct
/// and transitive drvs
fn drv_requisites(drv_path: &str) -> Result<Vec<DrvId>> {
    use std::str::FromStr;

    let output = Command::new("nix-store")
        .args(["--query", "--requisites", drv_path])
        .output()?
        .stdout;
    let drv_str = String::from_utf8(output)?;

    let drvs = drv_str
        .lines()
        // drv requisites can include "inputSrcs" which are not inputDrvs
        // but rather files which were added to the nix store through
        // path literals or `nix-store --add`
        .filter(|x| x.ends_with(".drv"))
        .chain(std::iter::once(drv_path))
        .map(|x| DrvId::from_str(x).unwrap())
        .collect::<Vec<DrvId>>();

    Ok(drvs)
}

// Retreive the direct dependencies of a drv
//fn drv_references(drv_path: &str) -> Result<Vec<String>> {
//    let output = Command::new("nix-store")
//        .args(["--query", "--references", drv_path])
//        .output()?
//        .stdout;
//    let drv_str = String::from_utf8(output)?;
//
//    let drvs = drv_str
//        .lines()
//        // drv references can include "inputSrcs" which are not inputDrvs
//        // but rather files which were added to the nix store through
//        // path literals or `nix-store --add`
//        .filter(|x| x.ends_with(".drv"))
//        .map(|x| x.to_string())
//        .collect::<Vec<String>>();
//
//    Ok(drvs)
//}

fn graph_line_to_drvids(drv_line: &str) -> Result<(DrvId, DrvId)> {
    let mut line = drv_line.split(" ");
    let reference: DrvId = graph_str_to_drvid(line.next().unwrap())?;
    // drop inner "->"
    line.next().unwrap();
    let referrer = graph_str_to_drvid(line.next().unwrap())?;

    Ok((reference, referrer))
}

/// This assumes a well-formated string from the output of nix-store --query --graph
fn graph_str_to_drvid(drv_str: &str) -> Result<DrvId> {
    use std::str::FromStr;

    use anyhow::bail;

    let mut reference_string: String = drv_str.to_string();
    reference_string.retain(|c| c != '"');
    if !reference_string.ends_with(".drv") {
        bail!("not a drv");
    }
    DrvId::from_str(&reference_string)
}

/// Retreive the entirity of a drv's reference graph.
/// This uses `nix-store --query --graph` to construct
/// the whole graph in one invocation
/// Returns: Vec<(reference, referrer)>, where the referrer consumes (downstream of) a reference
fn drv_reference_graph(drv_path: &str) -> Result<Vec<(DrvId, DrvId)>> {
    let output = Command::new("nix-store")
        .args(["--query", "--graph", drv_path])
        .output()?
        .stdout;
    let drv_str = String::from_utf8(output)?;

    let drvs = drv_str
        .lines()
        // The graph includes inputSrcs as well as graphviz node information
        // Filtering by " -> " assures we are only grabbing edges
        .filter(|x| x.contains(" -> "))
        .filter_map(|x| graph_line_to_drvids(x).ok())
        .collect::<Vec<(_, _)>>();

    debug!("drv_graph: {:?}", drvs);

    Ok(drvs)
}
