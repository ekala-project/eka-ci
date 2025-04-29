pub mod jobs;
pub mod nix_eval_jobs;

use crate::db::DbService;
use anyhow::Result;
use std::collections::HashMap;
use std::process::Command;
use tokio::sync::mpsc::Receiver;
use tracing::{debug, warn};

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
    drv_receiver: Receiver<EvalTask>,
    // TODO: Eventually this should be an LRU cache
    // This allows for us to memoize visited drvs so we don't have to revisit
    // common drvs (e.g. stdenv)
    drv_map: HashMap<String, Vec<String>>,
}

impl EvalService {
    pub fn new(rcvr: Receiver<EvalTask>, db_service: DbService) -> EvalService {
        EvalService {
            db_service,
            drv_receiver: rcvr,
            drv_map: HashMap::new(),
        }
    }

    pub fn run(self) {
        tokio::spawn(async {
            self.listen().await;
        });
    }

    async fn listen(mut self) {
        loop {
            match self.drv_receiver.recv().await {
                Some(EvalTask::Job(drv)) => {
                    if let Err(e) = self.run_nix_eval_jobs(drv.file_path).await {
                        warn!("Ran into error when query eval job: {}", e);
                    };
                }
                Some(EvalTask::TraverseDrv(drv)) => {
                    if let Err(e) = self.traverse_drvs(&drv).await {
                        warn!("Ran into error when query drv information: {}", e);
                    }
                }
                None => {
                    warn!("Eval reciever channel shutdown");
                }
            }
        }
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
        let mut new_drvs: HashMap<String, Vec<String>> = HashMap::new();

        debug!("traversing {}", drv_path);
        self.inner_traverse_drvs(drv_path, &mut new_drvs)?;
        self.db_service.insert_drv_graph(new_drvs).await?;

        Ok(())
    }

    /// To avoid lifetime issues, we do a recursive descent
    /// instead of appending to an iterator and a loop :(
    fn inner_traverse_drvs(
        &mut self,
        drv_path: &str,
        new_drvs: &mut HashMap<String, Vec<String>>,
    ) -> Result<()> {
        let references = drv_references(drv_path)?;
        debug!("new drv, traversing {}", &drv_path);
        self.drv_map
            .insert(drv_path.to_string(), references.clone());
        new_drvs.insert(drv_path.to_string(), references.clone());

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
