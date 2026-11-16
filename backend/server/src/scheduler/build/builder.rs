use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::process::Command;
use tokio::sync::mpsc::{self, Sender};
use tokio_util::sync::CancellationToken;
use tracing::info;

/// Timeout for quick nix operations (store ping, config show).
const NIX_QUICK_TIMEOUT: Duration = Duration::from_secs(30);

use super::builder_thread::BuilderThread;
use super::circuit_breaker::CircuitBreakerRegistry;
use super::{BuildRequest, Platform};
use crate::config::RemoteBuilder;
use crate::graph::GraphServiceHandle;
use crate::metrics::BuildMetrics;
use crate::nix::reconstitute::ReconstitutionTracker;
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
    pub supported_features: Vec<String>,
    pub mandatory_features: Vec<String>,
    logs_dir: PathBuf,
    recorder_sender: mpsc::Sender<RecorderTask>,
    metrics: Arc<BuildMetrics>,
    no_output_timeout_seconds: u64,
    max_duration_seconds: u64,
    graph_handle: GraphServiceHandle,
    db_pool: sqlx::SqlitePool,
    reconstitution_tracker: Arc<ReconstitutionTracker>,
    pub(in crate::scheduler::build) circuit_breaker: CircuitBreakerRegistry,
}

impl Builder {
    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        is_local: bool,
        max_jobs: u8,
        remote_uri: Option<String>,
        builder_name: String,
        platform: Platform,
        supported_features: Vec<String>,
        mandatory_features: Vec<String>,
        logs_dir: PathBuf,
        recorder_sender: Sender<RecorderTask>,
        metrics: Arc<BuildMetrics>,
        no_output_timeout_seconds: u64,
        max_duration_seconds: u64,
        graph_handle: GraphServiceHandle,
        db_pool: sqlx::SqlitePool,
        reconstitution_tracker: Arc<ReconstitutionTracker>,
        circuit_breaker: CircuitBreakerRegistry,
    ) -> Self {
        Self {
            is_local,
            max_jobs,
            remote_uri,
            builder_name,
            platform,
            supported_features,
            mandatory_features,
            logs_dir,
            recorder_sender,
            metrics,
            no_output_timeout_seconds,
            max_duration_seconds,
            graph_handle,
            db_pool,
            reconstitution_tracker,
            circuit_breaker,
        }
    }

    pub fn is_local(&self) -> bool {
        self.is_local
    }

    pub fn recorder_sender(&self) -> &mpsc::Sender<RecorderTask> {
        &self.recorder_sender
    }

    /// Check to see if remote builder/store is even available
    /// This prevent scheduling builds for a remote builder which
    /// will just fail because it's not available
    pub async fn is_available(&self) -> bool {
        if self.is_local {
            return true;
        }

        let remote_uri = match &self.remote_uri {
            Some(uri) => uri,
            None => return false,
        };

        tokio::time::timeout(
            NIX_QUICK_TIMEOUT,
            Command::new("nix")
                .kill_on_drop(true)
                .args(["store", "ping", "--store", remote_uri])
                .output(),
        )
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|x| x.status.success())
        .unwrap_or(false)
    }

    pub fn run(self, cancellation_token: CancellationToken) -> mpsc::Sender<BuildRequest> {
        let thread = BuilderThread::init(
            self.build_args(),
            self.max_jobs,
            self.logs_dir,
            self.recorder_sender.clone(),
            self.platform.clone(),
            self.metrics.clone(),
            self.no_output_timeout_seconds,
            self.max_duration_seconds,
            self.graph_handle.clone(),
            self.db_pool.clone(),
            self.reconstitution_tracker.clone(),
            self.builder_name.clone(),
            self.is_local,
            self.circuit_breaker.clone(),
        );

        thread.run(cancellation_token)
    }

    pub async fn local_from_env(
        logs_dir: PathBuf,
        recorder_sender: mpsc::Sender<RecorderTask>,
        metrics: Arc<BuildMetrics>,
        no_output_timeout_seconds: u64,
        max_duration_seconds: u64,
        graph_handle: GraphServiceHandle,
        db_pool: sqlx::SqlitePool,
        reconstitution_tracker: Arc<ReconstitutionTracker>,
        circuit_breaker: CircuitBreakerRegistry,
    ) -> Result<Vec<Self>> {
        let local_platforms = local_platforms().await?;
        let local_features = local_system_features().await?;

        info!(
            "Creating a local builder for these systems: {:?}",
            &local_platforms
        );
        info!("Local system features: {:?}", &local_features);

        let builders = local_platforms
            .iter()
            .map(|platform| {
                Self::new_inner(
                    true,
                    40,
                    None,
                    "localhost".to_string(),
                    platform.to_string(),
                    local_features.clone(),
                    Vec::new(), // No mandatory features for local builders
                    logs_dir.clone(),
                    recorder_sender.clone(),
                    metrics.clone(),
                    no_output_timeout_seconds,
                    max_duration_seconds,
                    graph_handle.clone(),
                    db_pool.clone(),
                    reconstitution_tracker.clone(),
                    circuit_breaker.clone(),
                )
            })
            .collect();

        Ok(builders)
    }

    pub async fn local_from_env_fod(
        logs_dir: PathBuf,
        recorder_sender: mpsc::Sender<RecorderTask>,
        metrics: Arc<BuildMetrics>,
        no_output_timeout_seconds: u64,
        max_duration_seconds: u64,
        graph_handle: GraphServiceHandle,
        db_pool: sqlx::SqlitePool,
        reconstitution_tracker: Arc<ReconstitutionTracker>,
        circuit_breaker: CircuitBreakerRegistry,
    ) -> Result<Vec<Self>> {
        let local_platforms = local_platforms().await?;
        let local_features = local_system_features().await?;

        info!(
            "Creating FOD builders for these systems: {:?}",
            &local_platforms
        );

        let builders = local_platforms
            .iter()
            .map(|platform| {
                Self::new_inner(
                    true,
                    10, // Lower max_jobs for FOD builds
                    None,
                    "localhost-fod".to_string(),
                    platform.to_string(),
                    local_features.clone(),
                    Vec::new(), // No mandatory features for local builders
                    logs_dir.clone(),
                    recorder_sender.clone(),
                    metrics.clone(),
                    no_output_timeout_seconds,
                    max_duration_seconds,
                    graph_handle.clone(),
                    db_pool.clone(),
                    reconstitution_tracker.clone(),
                    circuit_breaker.clone(),
                )
            })
            .collect();

        Ok(builders)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_remote_builder(
        platform: Platform,
        remote_builder: &RemoteBuilder,
        logs_dir: PathBuf,
        recorder_sender: mpsc::Sender<RecorderTask>,
        metrics: Arc<BuildMetrics>,
        no_output_timeout_seconds: u64,
        max_duration_seconds: u64,
        graph_handle: GraphServiceHandle,
        db_pool: sqlx::SqlitePool,
        reconstitution_tracker: Arc<ReconstitutionTracker>,
        circuit_breaker: CircuitBreakerRegistry,
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
            remote_builder.supported_features.clone(),
            remote_builder.mandatory_features.clone(),
            logs_dir,
            recorder_sender,
            metrics,
            no_output_timeout_seconds,
            max_duration_seconds,
            graph_handle,
            db_pool,
            reconstitution_tracker,
            circuit_breaker,
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
async fn nix_config_show() -> Result<String> {
    let config_output = tokio::time::timeout(
        NIX_QUICK_TIMEOUT,
        Command::new("nix")
            .kill_on_drop(true)
            .args(["config", "show"])
            .output(),
    )
    .await
    .context("nix config show timed out")?
    .context("failed to run nix config show")?;

    Ok(String::from_utf8(config_output.stdout)?)
}

/// Platforms the local nix daemon can build: `system` plus `extra-platforms`
async fn local_platforms() -> Result<Vec<String>> {
    Ok(parse_local_platforms(&nix_config_show().await?))
}

/// Query the local system features from nix config
/// These are features like kvm, nixos-test, big-parallel, benchmark that
/// the local system supports
async fn local_system_features() -> Result<Vec<String>> {
    Ok(config_values(&nix_config_show().await?, "system-features"))
}

/// Whitespace-separated values of `key` from `nix config show` output
fn config_values(config: &str, key: &str) -> Vec<String> {
    let prefix = format!("{key} =");
    config
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .flat_map(str::split_whitespace)
        .map(str::to_string)
        .collect()
}

/// `system` followed by `extra-platforms`, without duplicates
fn parse_local_platforms(config: &str) -> Vec<String> {
    let mut platforms: Vec<String> = Vec::new();
    let candidates = config_values(config, "system")
        .into_iter()
        .chain(config_values(config, "extra-platforms"));
    for platform in candidates {
        if !platforms.contains(&platform) {
            platforms.push(platform);
        }
    }
    platforms
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "\
experimental-features = flakes nix-command
extra-platforms = i686-linux x86_64-v1-linux x86_64-linux
system = x86_64-linux
system-features = benchmark big-parallel kvm nixos-test
";

    #[test]
    fn platforms_include_extra_platforms() {
        assert_eq!(
            parse_local_platforms(CONFIG),
            vec!["x86_64-linux", "i686-linux", "x86_64-v1-linux"]
        );
    }

    #[test]
    fn platforms_without_extra_platforms() {
        assert_eq!(
            parse_local_platforms("system = aarch64-linux\nextra-platforms = \n"),
            vec!["aarch64-linux"]
        );
    }

    #[test]
    fn system_features_parsed() {
        assert_eq!(
            config_values(CONFIG, "system-features"),
            vec!["benchmark", "big-parallel", "kvm", "nixos-test"]
        );
    }
}
