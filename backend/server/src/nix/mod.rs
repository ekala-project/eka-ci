mod nix_eval_jobs;

use anyhow::Result;
use std::collections::HashMap;
use std::process::Command;
use tokio::sync::mpsc::Receiver;
use tracing::{debug, warn};

pub struct EvalService {
    drv_receiver: Receiver<String>,
    // TODO: Eventually this should be an LRU cache
    drv_map: HashMap<String, Vec<String>>,
}

impl EvalService {
    pub fn new(rcvr: Receiver<String>) -> EvalService {
        EvalService {
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
                Some(drv) => {
                    if let Err(e) = self.traverse_drvs(&drv) {
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
    fn traverse_drvs(&mut self, drv_path: &str) -> Result<()> {
        if self.drv_map.contains_key(drv_path) {
            debug!("Already evaluated {}, skipping....", drv_path);
            return Ok(());
        }

        debug!("traversing {}", drv_path);
        self.inner_traverse_drvs(drv_path)?;

        Ok(())
    }

    /// To avoid lifetime issues, we do a recursive descent instead of a loop
    /// instead of appending to an iterator :(
    fn inner_traverse_drvs(&mut self, drv_path: &str) -> Result<()> {
        let references = drv_references(drv_path)?;
        // TODO: this drv hasn't been built before, it should eventually it put into a "new drv"
        // queue
        debug!("new drv, traversing {}", &drv_path);
        self.drv_map
            .insert(drv_path.to_string(), references.clone());

        for drv in references.into_iter() {
            if self.drv_map.contains_key(&drv) {
                continue;
            }
            self.inner_traverse_drvs(&drv)?;
        }

        Ok(())
    }
}

/// Retreive the direct dependencies of a drv
fn drv_references(drv_path: &str) -> Result<Vec<String>> {
    let output = Command::new("nix-store")
        .args(&["--query", "--references", &drv_path])
        .output()?
        .stdout;
    let drv_str = String::from_utf8(output)?;

    let drvs = drv_str
        .lines()
        // drv references can include "inputSrcs" which are not inputDrvs
        // but rather files which were added to the nix store through `nix-store --add`
        .filter(|x| x.ends_with(".drv"))
        .map(|x| x.to_string())
        .collect::<Vec<String>>();

    Ok(drvs)
}
