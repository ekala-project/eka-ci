pub mod derivation_show;
pub mod jobs;
pub mod nix_eval_jobs;

use crate::db::{model::drv_id::DrvId, DbService};
use crate::scheduler::IngressTask;
use anyhow::Result;
use std::{collections::HashMap, process::Command};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

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
    drv_map: HashMap<String, Vec<String>>,
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
                }
            };

            let result = match task {
                EvalTask::Job(drv) => self.run_nix_eval_jobs(drv.file_path).await,
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
        debug!("Entering traverse drvs");
        if self.drv_map.contains_key(drv_path) || self.db_service.has_drv(drv_path).await? {
            debug!("Already evaluated {}, skipping....", drv_path);
            return Ok(());
        }

        // This is used to collect drvs for insertion into the database
        // We must know all of the drvs before refrencing relationships
        // So we must complete the traversal, then attempt assertion of
        // drvs (which are the keys in this case), then can add the references
        let mut new_drvs: HashMap<DrvId, Vec<DrvId>> = HashMap::new();

        debug!("traversing {}", drv_path);

        // Populate new_drvs with any drvs that haven't been previously
        // visited
        self.inner_traverse_drvs(drv_path, &mut new_drvs)?;
        // Populate DB with drvs and their dependency graph
        // This is necessary to ensure that we don't progress to builds
        // which have yet had their dependencies entered into the DB
        self.db_service.insert_drv_graph(&new_drvs).await?;
        // Ask scheduler service to determine if it needs to
        // build a drv
        for (drv, _) in new_drvs {
            self.scheduler_sender
                .send(IngressTask::EvalRequest(drv))
                .await?;
        }

        Ok(())
    }

    /// To avoid lifetime issues, we do a recursive descent
    /// instead of appending to an iterator and a loop :(
    fn inner_traverse_drvs(
        &mut self,
        drv_path: &str,
        new_drvs: &mut HashMap<DrvId, Vec<DrvId>>,
    ) -> Result<()> {
        let references = drv_references(drv_path)?;
        debug!("new drv, traversing {}", &drv_path);
        self.drv_map
            .insert(drv_path.to_string(), references.clone());
        new_drvs.insert(
            drv_path.try_into()?,
            references
                .iter()
                .map(|s| DrvId::try_from(s.as_str()))
                .collect::<Result<_, _>>()?,
        );

        for drv in references.into_iter() {
            if self.drv_map.contains_key(&drv) {
                continue;
            }
            self.inner_traverse_drvs(&drv, new_drvs)?;
        }

        Ok(())
    }
}

/// Retreive the direct dependencies of a drv
fn drv_references(drv_path: &str) -> Result<Vec<String>> {
    let output = Command::new("nix-store")
        .args(["--query", "--references", drv_path])
        .output()?
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

/// This assumes a well-formated string from the output of nix-store --query --graph
fn graph_str_to_drvid(drv_str: &str) -> DrvId {
    use std::str::FromStr;

    let mut reference_string: String = drv_str.to_string();
    reference_string.retain(|c| c != '"');
    DrvId::from_str(&reference_string).unwrap()
}

/// Retreive the entirity of a drv's reference graph.
/// This uses `nix-store --query --graph` to construct
/// the whole graph in one invocation
fn drv_reference_graph(drv_path: &str) -> Result<Vec<(DrvId,DrvId)>> {
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
        .map(|x| {
            let mut line = x.split(" ");
            let reference: DrvId = graph_str_to_drvid(line.next().unwrap());
            // drop inner "->"
            line.next();
            let referrer = graph_str_to_drvid(line.next().unwrap());
            return (reference, referrer)
        })
        .collect::<Vec<(_,_)>>();

    Ok(drvs)
}
