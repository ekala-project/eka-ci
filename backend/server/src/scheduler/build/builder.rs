use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use tokio::process::Command;
use tokio::sync::mpsc::{self, Sender};
use tracing::info;

use super::builder_thread::BuilderThread;
use super::{BuildRequest, Platform};
use crate::config::RemoteBuilder;
use crate::metrics::BuildMetrics;
use crate::scheduler::recorder::RecorderTask;

/// This is meant to be an abstraction over both local and remote builders
///
/// Combining the two usages is a bit ugly, but it makes calling code much
/// simpler
pub struct Builder {
    is_local: bool,
    pub max_jobs: u8,
    pub remote_uri: Option<String>,
    pub builder_name: String,
    pub platform: Platform,
    logs_dir: PathBuf,
    recorder_sender: mpsc::Sender<RecorderTask>,
    metrics: Arc<BuildMetrics>,
    no_output_timeout_seconds: u64,
}

impl Builder {
    fn new_inner(
        is_local: bool,
        max_jobs: u8,
        remote_uri: Option<String>,
        builder_name: String,
        platform: Platform,
        logs_dir: PathBuf,
        recorder_sender: Sender<RecorderTask>,
        metrics: Arc<BuildMetrics>,
        no_output_timeout_seconds: u64,
    ) -> Self {
        Self {
            is_local,
            max_jobs,
            remote_uri,
            builder_name,
            platform,
            logs_dir,
            recorder_sender,
            metrics,
            no_output_timeout_seconds,
        }
    }

    pub fn is_local(&self) -> bool {
        self.is_local
    }

    /// Check to see if remote builder/store is even available
    /// This prevent scheduling builds for a remote builder which
    /// will just fail because it's not available
    pub async fn is_available(&self) -> bool {
        if self.is_local {
            return true;
        }

        Command::new("nix")
            .args([
                "store",
                "ping",
                "--store",
                self.remote_uri.as_ref().unwrap(),
            ])
            .output()
            .await
            .map(|x| x.status.success())
            .unwrap_or(false)
    }

    pub fn run(self) -> mpsc::Sender<BuildRequest> {
        let thread = BuilderThread::init(
            self.build_args(),
            self.max_jobs,
            self.logs_dir,
            self.recorder_sender.clone(),
            self.platform.clone(),
            self.metrics.clone(),
            self.no_output_timeout_seconds,
        );

        thread.run()
    }

    pub async fn local_from_env(
        logs_dir: PathBuf,
        recorder_sender: mpsc::Sender<RecorderTask>,
        metrics: Arc<BuildMetrics>,
        no_output_timeout_seconds: u64,
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
                    40,
                    None,
                    "localhost".to_string(),
                    platform.to_string(),
                    logs_dir.clone(),
                    recorder_sender.clone(),
                    metrics.clone(),
                    no_output_timeout_seconds,
                )
            })
            .collect();

        Ok(builders)
    }

    pub fn from_remote_builder(
        platform: Platform,
        remote_builder: &RemoteBuilder,
        logs_dir: PathBuf,
        recorder_sender: mpsc::Sender<RecorderTask>,
        metrics: Arc<BuildMetrics>,
        no_output_timeout_seconds: u64,
    ) -> Self {
        Self::new_inner(
            false,
            remote_builder.max_jobs,
            Some(remote_builder.uri.clone()),
            format!(
                "'{} {}'",
                remote_builder.uri,
                remote_builder.platforms.join(",")
            ),
            platform,
            logs_dir,
            recorder_sender,
            metrics,
            no_output_timeout_seconds,
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

    let system_str = system_line.split(" ").nth(2).unwrap();

    let systems = system_str.split(",").map(|x| x.to_string()).collect();

    Ok(systems)
}
