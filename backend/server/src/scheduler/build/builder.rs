use anyhow::Result;
use tokio::process::Command;
use tokio::sync::mpsc::{self, Sender};
use tokio::task::JoinHandle;
use tracing::info;

use super::builder_thread::BuilderThread;
use super::{BuildRequest, Platform};
use crate::config::RemoteBuilder;
use crate::db::DbService;
use crate::scheduler::recorder::RecorderTask;

/// This is meant to be an abstraction over both local and remote builders
///
/// Combining the two usages is a bit ugly, but it makes calling code much
/// simpler
pub struct Builder {
    is_local: bool,
    max_jobs: u8,
    pub builder_name: String,
    pub platform: Platform,
    db_service: DbService,
    recorder_sender: mpsc::Sender<RecorderTask>,
}

impl Builder {
    fn new_inner(
        is_local: bool,
        max_jobs: u8,
        builder_name: String,
        platform: Platform,
        db_service: DbService,
        recorder_sender: Sender<RecorderTask>,
    ) -> Self {
        Self {
            is_local,
            max_jobs,
            builder_name,
            platform,
            db_service,
            recorder_sender,
        }
    }

    pub fn run(self) -> mpsc::Sender<BuildRequest> {
        let thread = BuilderThread::init(
            self.build_args(),
            self.max_jobs,
            self.db_service.clone(),
            self.recorder_sender.clone(),
        );

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
            .map(|platform| {
                Self::new_inner(
                    true,
                    4,
                    "localhost".to_string(),
                    platform.to_string(),
                    db_service.clone(),
                    recorder_sender.clone(),
                )
            })
            .collect();

        Ok(builders)
    }

    pub fn from_remote_builder(
        platform: Platform,
        remote_builder: &RemoteBuilder,
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

    fn build_args(&self) -> [String; 2] {
        if self.is_local {
            // Force the build command to not use remote builders
            ["--builders".to_string(), "''".to_string()]
        } else {
            ["--builders".to_string(), self.builder_name.clone()]
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
