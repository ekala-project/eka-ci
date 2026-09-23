use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;

use builder_proto::builder_service_server::BuilderService;
use builder_proto::{
    BuilderMessage, JoinRequest, JoinResponse, LogAck, LogChunk, SchedulerMessage,
};
use tokio::sync::{Mutex, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status, Streaming};
use tracing::info;

type GrpcResult<T> = Result<Response<T>, Status>;
type ResponseStream =
    Pin<Box<dyn tokio_stream::Stream<Item = Result<SchedulerMessage, Status>> + Send>>;

/// Tracks a connected builder's state.
#[allow(dead_code)]
struct ConnectedBuilder {
    hostname: String,
    systems: Vec<String>,
    max_jobs: u32,
    supported_features: Vec<String>,
    mandatory_features: Vec<String>,
    /// Channel for sending SchedulerMessages (build tasks, pings) to this builder.
    task_sender: mpsc::Sender<Result<SchedulerMessage, Status>>,
}

/// gRPC service implementation hosted by the scheduler.
///
/// Remote builder agents connect via Join + OpenTunnel. The scheduler
/// tracks connected builders and can dispatch build work to them.
pub struct BuilderGrpcService {
    builders: Arc<Mutex<HashMap<String, ConnectedBuilder>>>,
}

impl BuilderGrpcService {
    pub fn new() -> Self {
        Self {
            builders: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Get the list of currently connected builder IDs.
    pub async fn connected_builders(&self) -> Vec<String> {
        self.builders.lock().await.keys().cloned().collect()
    }
}

#[tonic::async_trait]
impl BuilderService for BuilderGrpcService {
    type OpenTunnelStream = ResponseStream;

    async fn join(&self, request: Request<JoinRequest>) -> GrpcResult<JoinResponse> {
        let req = request.into_inner();
        info!(
            "builder '{}' ({}) joining with systems={:?}, max_jobs={}",
            req.builder_id, req.hostname, req.systems, req.max_jobs
        );

        let response = JoinResponse {
            builder_id: req.builder_id.clone(),
            session_token: uuid::Uuid::new_v4().to_string(),
            granted_max_jobs: req.max_jobs,
        };

        Ok(Response::new(response))
    }

    async fn open_tunnel(
        &self,
        request: Request<Streaming<BuilderMessage>>,
    ) -> GrpcResult<Self::OpenTunnelStream> {
        let mut stream = request.into_inner();

        // Channel for sending scheduler messages back to the builder.
        let (tx, rx) = mpsc::channel(64);
        let builders = self.builders.clone();

        tokio::spawn(async move {
            let mut builder_id: Option<String> = None;

            while let Ok(Some(msg)) = stream.message().await {
                match msg.message {
                    Some(builder_proto::builder_message::Message::Heartbeat(hb)) => {
                        if builder_id.is_none() {
                            builder_id = Some(hb.builder_id.clone());
                            let connected = ConnectedBuilder {
                                hostname: String::new(),
                                systems: Vec::new(),
                                max_jobs: 0,
                                supported_features: Vec::new(),
                                mandatory_features: Vec::new(),
                                task_sender: tx.clone(),
                            };
                            builders
                                .lock()
                                .await
                                .insert(hb.builder_id.clone(), connected);
                            info!(
                                "builder '{}' tunnel opened (load1={:.1})",
                                hb.builder_id, hb.load1
                            );
                        }
                    },
                    Some(builder_proto::builder_message::Message::BuildReport(report)) => {
                        info!(
                            "build report: id={} outcome={:?} duration={}ms",
                            report.build_id, report.outcome, report.duration_ms
                        );
                    },
                    None => {},
                }
            }

            // Builder disconnected — remove from registry.
            if let Some(id) = builder_id {
                builders.lock().await.remove(&id);
                info!("builder '{}' disconnected", id);
            }
        });

        let output = ReceiverStream::new(rx);
        Ok(Response::new(Box::pin(output)))
    }

    async fn stream_log(&self, request: Request<Streaming<LogChunk>>) -> GrpcResult<LogAck> {
        let mut stream = request.into_inner();

        while let Ok(Some(chunk)) = stream.message().await {
            // In a full implementation, write to the build log store.
            tracing::debug!(
                "log chunk: build_id={} bytes={}",
                chunk.build_id,
                chunk.data.len()
            );
        }

        Ok(Response::new(LogAck {}))
    }
}
