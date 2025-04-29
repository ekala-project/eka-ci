use crate::nix::nix_eval_jobs::NixEvalItem;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use tracing::{debug, warn};

/// This file is meant to handle the evaluation of a "job" which is similar
/// to the "jobset" by hydra, in particular:
/// - You pass the file path of a nix file
/// - You can optionally pass arguments to the file, which should be structured
///   as a function which receives an attrset of inputs
/// - The file outputs an [deeply nested] attrset of attrset<attr_path, drv>

impl super::EvalService {
    pub async fn run_nix_eval_jobs(&mut self, file_path: String) -> anyhow::Result<()> {
        let mut cmd = Command::new("nix-eval-jobs")
            .arg(file_path)
            .stdout(Stdio::piped())
            .spawn()?;

        {
            // TODO: handle failure case more nicely
            let stdout = cmd.stdout.as_mut().unwrap();
            // Create a stream, so that we can pass through values as they are produced
            let stdout_reader = BufReader::new(stdout);
            let stdout_lines = stdout_reader.lines();

            for line in stdout_lines {
                if let Ok(input) = line {
                    let item = serde_json::from_str::<NixEvalItem>(&input)?;
                    match item {
                        NixEvalItem::Drv(drv) => {
                            if let Err(e) = self.traverse_drvs(&drv.drv_path).await {
                                warn!("Issue while traversing {} drv: {:?}", &drv.drv_path, e);
                            };
                        }
                        NixEvalItem::Error(e) => {
                            // TODO: Collect evaluation errors, these are still very useful
                            debug!("error: {:?}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
