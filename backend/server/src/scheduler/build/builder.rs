use anyhow::Result;
use tracing::{debug, info, warn};

use super::BuildRequest;
use super::Platform;
use crate::config::RemoteBuilder;
use tokio::process::Command;
use tokio::sync::mpsc::{self, Receiver, Sender};

/// This is meant to be an abstraction over both local and remote builders
///
/// Combining the two usages is a bit ugly, but it makes calling code much
/// simpler
pub struct Builder {
    is_local: bool,
    max_jobs: u8,
    pub builder_name: String,
    pub platform: Platform,
    build_thread: JoinHandle<()>,
    db_service: DbService,
    recorder_sender: mpsc::Sender<RecorderTask>,
}

impl Builder {
    fn new_inner(is_local: bool, max_jobs: u8, builder_name: String, platform: Platform) -> Self {
        Self {
            is_local,
            max_jobs,
            builder_name,
            platform,
        }
    }

    pub fn run(self) -> mpsc::Sender<BuildRequest> {
      let thread = BuilderThread {
          build_args: self.build_args(),
          max_jobs: self.max_jobs,
          db_service: self.db_service,
          recorder_sender: self.recorder_sender,
      };

      thread.run()
    }

    pub async fn local_from_env(
        db_service: DbService,
        recorder_sender: mpsc::Sender<RecorderTask>,
        ) -> Result<Vec<Self>> {
        let local_platforms = local_platforms().await?;

        info!(
            "Creating a local builder for these systems: {:?}",
            &local_platforms
        );

        let builders = local_platforms
            .iter()
            .map(|platform| Self::new_inner(true, 4, "localhost".to_string(), platform.to_string(), db_service, recorder_sender))
            .collect();

        Ok(builders)
    }

    pub fn from_remote_builder(
        platform: Platform,
        remote_builder: &RemoteBuilder
        db_service: DbService,
        recorder_sender: mpsc::Sender<RecorderTask>,
    ) -> Self {
        Self::new_inner(
            false,
            remote_builder.max_jobs,
            format!(
                "'{} {}'",
                remote_builder.uri,
                remote_builder.platforms.join(",")
            ),
            platform,
            db_service,
            recorder_sender,
        )
    }

    fn build_args(&self) -> [&str; 2] {
        if self.is_local {
            // Force the build command to not use remote builders
            ["--builders", "''"]
        } else {
            ["--builders", &self.builder_name]
        }
    }
}

/// Nix conf value merging is actually quite complex with user and system
/// settings. Just read the output from the 'nix config show' command to deterimne values
async fn local_platforms() -> Result<Vec<String>> {
    let config_output = Command::new("nix")
        .args(["config", "show"])
        .output()
        .await?;

    let config_str = String::from_utf8(config_output.stdout)?;

    let system_line: String = config_str
        .lines()
        .filter(|x| x.starts_with("system ="))
        .collect();

    let system_str = system_line.split(" ").skip(2).next().unwrap();

    let systems = system_str.split(",").map(|x| x.to_string()).collect();

    Ok(systems)
}
