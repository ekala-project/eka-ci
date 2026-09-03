// Build event recorder service

use std::sync::Arc;

use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::channels::types::ChannelTask;
use crate::db::DbService;
use crate::db::model::{build_event, drv_id};
use crate::github::GitHubTask;
use crate::graph::{GraphCommand, GraphServiceHandle};
use crate::hooks::types::HookTask;
use crate::scheduler::ingress::IngressTask;
use crate::services::TaskJournal;
use crate::services::websocket::events::ServerEvent;

// Sub-modules
mod forge_notify;
mod hooks;
mod references;
mod request_handler;
mod size_checks;
mod state;

/// `derivation` is `Arc<DrvId>` so `handle_recorder_request` can fan out the
/// drv id into multiple downstream tasks (ingress requeue, github status,
/// websocket event) via cheap `Arc::clone` refcount bumps.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// Producer side of the release-channel mpsc. Optional so test
    /// rigs and deployments that do not configure channels can still
    /// build a recorder; `None` simply suppresses all
    /// `ChannelTask::JobsetComplete` emissions.
    channel_sender: Option<mpsc::Sender<ChannelTask>>,
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
    channel_sender: Option<mpsc::Sender<ChannelTask>>,
    journal: TaskJournal<RecorderTask>,
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
        channel_sender: Option<mpsc::Sender<ChannelTask>>,
    ) -> (Self, mpsc::Sender<RecorderTask>) {
        let (recorder_sender, recorder_receiver) = mpsc::channel(1_000);

        let res = Self {
            db_service,
            recorder_receiver,
            github_sender,
            websocket_sender,
            graph_command_sender,
            graph_handle,
            hook_sender,
            cache_configs,
            channel_sender,
        };

        (res, recorder_sender)
    }

    pub fn run(
        self,
        ingress_sender: mpsc::Sender<IngressTask>,
        cancellation_token: CancellationToken,
    ) -> JoinHandle<()> {
        let pool = self.db_service.pool.clone();
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
            self.channel_sender,
            pool,
        );

        tokio::spawn(async move {
            worker.ingest_requests(cancellation_token).await;
        })
    }
}

impl RecorderWorker {
    #[allow(clippy::too_many_arguments)]
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
        channel_sender: Option<mpsc::Sender<ChannelTask>>,
        pool: sqlx::SqlitePool,
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
            channel_sender,
            journal: TaskJournal::new(pool, "recorder"),
        }
    }

    async fn ingest_requests(mut self, cancellation_token: CancellationToken) {
        // Replay un-acknowledged tasks from a previous crash.
        match self.journal.recover().await {
            Ok(recovered) => {
                for (jid, task) in recovered {
                    info!("replaying recovered recorder task: {:?}", &task);
                    if let Err(e) = self.handle_recorder_request(&task).await {
                        warn!(
                            "Failed to handle recovered recorder request {:?}: {:?}",
                            &task, e
                        );
                    } else if let Err(e) = self.journal.acknowledge(jid).await {
                        warn!("failed to ack recovered recorder journal entry: {:?}", e);
                    }
                }
            },
            Err(e) => warn!("failed to recover recorder journal: {:?}", e),
        }

        while let Some(request) = cancellation_token
            .run_until_cancelled(self.recorder_receiver.recv())
            .await
        {
            let task = match request {
                Some(task) => task,
                None => {
                    warn!("Recorder receiver channel closed, shutting down");
                    break;
                },
            };
            debug!("Received recorder task {:?}", &task);

            let jid = match self.journal.persist(&task).await {
                Ok(id) => Some(id),
                Err(e) => {
                    warn!("failed to journal recorder task {:?}: {:?}", &task, e);
                    None
                },
            };

            if let Err(e) = self.handle_recorder_request(&task).await {
                warn!("Failed to handle recorder request {:?}: {:?}", &task, e);
            } else if let Some(id) = jid {
                if let Err(e) = self.journal.acknowledge(id).await {
                    warn!("failed to ack recorder journal entry: {:?}", e);
                }
            }
        }

        info!("RecorderWorker service shutdown gracefully");
    }
}
