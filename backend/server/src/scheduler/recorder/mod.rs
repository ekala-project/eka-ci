// Build event recorder service

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::db::model::{build_event, drv_id};
use crate::github::GitHubTask;
use crate::graph::{GraphCommand, GraphServiceHandle};
use crate::hooks::types::HookTask;
use crate::scheduler::ingress::IngressTask;
use crate::services::websocket::events::ServerEvent;

// Sub-modules
mod hooks;
mod references;
mod request_handler;
mod size_checks;
mod state;

/// `derivation` is `Arc<DrvId>` so `handle_recorder_request` can fan out the
/// drv id into multiple downstream tasks (ingress requeue, github status,
/// websocket event) via cheap `Arc::clone` refcount bumps.
#[derive(Debug, Clone)]
pub struct RecorderTask {
    pub derivation: Arc<drv_id::DrvId>,
    pub result: build_event::DrvBuildState,
}

/// This services records the event of a build. Depending on the build result,
/// this service may also enqueue new ingress requests.
pub struct RecorderService {
    db_service: DbService,
    recorder_receiver: mpsc::Receiver<RecorderTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    websocket_sender: Option<broadcast::Sender<ServerEvent>>,
    graph_command_sender: mpsc::Sender<GraphCommand>,
    graph_handle: GraphServiceHandle,
    hook_sender: Option<mpsc::Sender<HookTask>>,
    cache_configs: std::sync::Arc<std::collections::HashMap<String, crate::config::CacheConfig>>,
}

/// Encapsulation of the Recorder thread. May want to gracefully recover from
/// any one particular thread going into a bad state
pub(super) struct RecorderWorker {
    /// To send buildable requests to builder service
    ingress_sender: mpsc::Sender<IngressTask>,
    recorder_receiver: mpsc::Receiver<RecorderTask>,
    db_service: DbService,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    websocket_sender: Option<broadcast::Sender<ServerEvent>>,
    graph_command_sender: mpsc::Sender<GraphCommand>,
    graph_handle: GraphServiceHandle,
    hook_sender: Option<mpsc::Sender<HookTask>>,
    cache_configs: std::sync::Arc<std::collections::HashMap<String, crate::config::CacheConfig>>,
}

impl RecorderService {
    pub fn init(
        db_service: DbService,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        websocket_sender: Option<broadcast::Sender<ServerEvent>>,
        graph_command_sender: mpsc::Sender<GraphCommand>,
        graph_handle: GraphServiceHandle,
        hook_sender: Option<mpsc::Sender<HookTask>>,
        cache_configs: std::sync::Arc<
            std::collections::HashMap<String, crate::config::CacheConfig>,
        >,
    ) -> (Self, mpsc::Sender<RecorderTask>) {
        let (recorder_sender, recorder_receiver) = mpsc::channel(1000);

        let res = Self {
            db_service,
            recorder_receiver,
            github_sender,
            websocket_sender,
            graph_command_sender,
            graph_handle,
            hook_sender,
            cache_configs,
        };

        (res, recorder_sender)
    }

    pub fn run(self, ingress_sender: mpsc::Sender<IngressTask>) -> JoinHandle<()> {
        let worker = RecorderWorker::new(
            self.db_service.clone(),
            ingress_sender,
            self.recorder_receiver,
            self.github_sender,
            self.websocket_sender,
            self.graph_command_sender,
            self.graph_handle,
            self.hook_sender,
            self.cache_configs,
        );

        tokio::spawn(async move {
            worker.ingest_requests().await;
        })
    }
}

impl RecorderWorker {
    fn new(
        db_service: DbService,
        ingress_sender: mpsc::Sender<IngressTask>,
        recorder_receiver: mpsc::Receiver<RecorderTask>,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        websocket_sender: Option<broadcast::Sender<ServerEvent>>,
        graph_command_sender: mpsc::Sender<GraphCommand>,
        graph_handle: GraphServiceHandle,
        hook_sender: Option<mpsc::Sender<HookTask>>,
        cache_configs: std::sync::Arc<
            std::collections::HashMap<String, crate::config::CacheConfig>,
        >,
    ) -> Self {
        Self {
            db_service,
            ingress_sender,
            recorder_receiver,
            github_sender,
            websocket_sender,
            graph_command_sender,
            graph_handle,
            hook_sender,
            cache_configs,
        }
    }

    async fn ingest_requests(mut self) {
        loop {
            if let Some(task) = self.recorder_receiver.recv().await {
                debug!("Received recorder task {:?}", &task);
                if let Err(e) = self.handle_recorder_request(&task).await {
                    warn!("Failed to handle ingress request {:?}: {:?}", &task, e);
                }
            }
        }
    }
}
