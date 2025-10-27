use anyhow::Result;
use tracing::{debug, warn, info};

use crate::config::RemoteBuilder;
use tokio::process::Command;
use tokio::sync::mpsc::{self, Sender, Receiver};
use super::Platform;
use super::BuildRequest;

/// This is meant to be an abstraction over both local and remote builders
///
/// Combining the two usages is a bit ugly, but it makes calling code much
/// simpler
pub struct Builder {
    is_local: bool,
    max_jobs: u8,
    build_sender: Sender<BuildRequest>,
    build_receiver: Receiver<BuildRequest>,
    pub builder_name: String,
    pub platform: Platform,
}

impl Builder {
    fn new_inner(
        is_local: bool,
        max_jobs: u8,
        builder_name: String,
        platform: Platform,
    ) -> Self {
        let (build_sender, build_receiver) = mpsc::channel(1);

        Self {
            is_local,
            max_jobs,
            build_sender,
            build_receiver,
            builder_name,
            platform,
        }
    }

    pub fn run(self) -> mpsc::Sender<BuildRequest> {
        let (tx, rx) = mpsc::channel(self.max_jobs.into());

        tokio::spawn(async move {
            self.loop_for_builds(rx).await;
        });

        tx
    }

    pub async fn local_from_env() -> Result<Vec<Self>> {
        let local_platforms = local_platforms().await?;

        info!("Creating a local builder for these systems: {:?}", &local_platforms);

        let builders = local_platforms.iter().map(|platform|
            Self::new_inner(true, 4, "localhost".to_string(), platform.to_string())
        ).collect();

        Ok(builders)
    }

    pub fn from_remote_builder(platform: Platform, remote_builder: &RemoteBuilder) -> Self {
        Self::new_inner(false,
            remote_builder.max_jobs,
            format!("'{} {}'", remote_builder.uri, remote_builder.platforms.join(",")),
            platform,
        )
    }

    fn build_args(&self) -> [&str;2] {
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

    let system_line: String = config_str.lines()
        .filter(|x| x.starts_with("system ="))
        .collect();

    let system_str = system_line.
        split(" ")
        .skip(2)
        .next().unwrap();

    let systems = system_str
        .split(",")
        .map(|x| x.to_string())
        .collect();

    Ok(systems)
}
