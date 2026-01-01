use std::path::PathBuf;
use std::sync::Arc;

use prometheus::Registry;
use prometheus::process_collector::ProcessCollector;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::build::{BuildQueue, BuildRequest, Builder};
use super::ingress::{IngressService, IngressTask};
use super::recorder::{RecorderService, RecorderTask};
use crate::config::RemoteBuilder;
use crate::db::DbService;
use crate::github::GitHubTask;
use crate::metrics::BuildMetrics;

/// SchedulerService spins up three smaller services:
///   RequestIngress:
///         Handles taking in many drv build requests and determines
///         if the drv is "buildable", has a failed dependency, otherwise it's just queued
///   BuildQueue:
///         Receives a stream of "buildable" drvs and builds them.
///         Upon completion of a build, passes build status to RecorderService
///   RecorderService:
///         "Records" build status to the database.
///         Upon recording a successful build, it pushes a check request for
///         each downstream drv to RequestIngress service to see if the drv
///         is now "buildable"
#[allow(dead_code)]
pub struct SchedulerService {
    // Handle to the db service, useful for persisting and querying build state
    db_service: DbService,

    // Prometheus metrics registry
    metrics_registry: Arc<Registry>,

    // Channels to individual services
    // We may in the future need to recover an individual service, so retaining
    // a handle to the other service channels will be prequisite
    ingress_sender: mpsc::Sender<IngressTask>,
    builder_sender: mpsc::Sender<BuildRequest>,
    recorder_sender: mpsc::Sender<RecorderTask>,

    ingress_thread: JoinHandle<()>,
    builder_thread: JoinHandle<()>,
    recorder_thread: JoinHandle<()>,
}

impl SchedulerService {
    pub async fn new(
        db_service: DbService,
        logs_dir: PathBuf,
        remote_builders: Vec<RemoteBuilder>,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        build_no_output_timeout_seconds: u64,
    ) -> anyhow::Result<Self> {
        // Create metrics registry and build metrics
        let metrics_registry = Arc::new(Registry::new());
        let build_metrics = BuildMetrics::new(&metrics_registry)?;

        // Register process metrics collector
        let process_collector = ProcessCollector::for_self();
        metrics_registry.register(Box::new(process_collector))?;

        let (ingress_service, ingress_sender) = IngressService::init(db_service.clone());
        let (recorder_service, recorder_sender) =
            RecorderService::init(db_service.clone(), github_sender);
        let mut builders = Builder::local_from_env(
            logs_dir.clone(),
            recorder_sender.clone(),
            build_metrics.clone(),
            build_no_output_timeout_seconds,
        )
        .await?;
        let fod_builders = Builder::local_from_env_fod(
            logs_dir.clone(),
            recorder_sender.clone(),
            build_metrics.clone(),
            build_no_output_timeout_seconds,
        )
        .await?;
        for remote in remote_builders {
            for remote_platform in &remote.platforms {
                let remote_builder = Builder::from_remote_builder(
                    remote_platform.to_string(),
                    &remote,
                    logs_dir.clone(),
                    recorder_sender.clone(),
                    build_metrics.clone(),
                    build_no_output_timeout_seconds,
                );
                builders.push(remote_builder);
            }
        }

        let (builder_service, builder_sender) =
            BuildQueue::init(builders, fod_builders, build_metrics).await;
        let ingress_thread = ingress_service.run(builder_sender.clone());
        let recorder_thread = recorder_service.run(ingress_sender.clone());
        let builder_thread = builder_service.run();

        Ok(Self {
            db_service,
            metrics_registry,
            ingress_sender,
            builder_sender,
            recorder_sender,
            ingress_thread,
            builder_thread,
            recorder_thread,
        })
    }

    /// Producer channel for sending build requests for eventual scheduling
    pub fn ingress_request_sender(&self) -> mpsc::Sender<IngressTask> {
        self.ingress_sender.clone()
    }

    /// Get the Prometheus metrics registry for exposing via HTTP
    pub fn metrics_registry(&self) -> Arc<Registry> {
        self.metrics_registry.clone()
    }
}
