use anyhow::{Context, Result};
use shared::types::{ClientRequest, ClientResponse};
use std::path::Path;
use tokio::sync::mpsc::Sender;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{unix::SocketAddr, UnixListener, UnixStream},
};
use tracing::{debug, info, warn};

pub struct UnixService {
    listener: UnixListener,
    /// Channel to emit drvs to be evaluated
    dispatch: DispatchChannels,
}

/// Channels which can be used to communicate actions to other services
#[derive(Clone)]
struct DispatchChannels {
    eval_sender: Sender<String>,
}

impl UnixService {
    // TODO: We should probably use a builder pattern to pass eval channel and other items
    pub async fn bind_to_path(socket_path: &Path, eval_sender: Sender<String>) -> Result<Self> {
        prepare_path(socket_path)?;

        let listener = UnixListener::bind(socket_path)?;
        let dispatch = DispatchChannels { eval_sender };

        Ok(Self { listener, dispatch })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        // If the call fails either the system ran out of resources or libc is broken, for both of
        // these cases a panic seems appropiate.
        self.listener
            .local_addr()
            .expect("getsockname should always succeed on a properly initialized listener")
    }

    pub async fn run(self) {
        self.listen_for_client().await
    }

    async fn listen_for_client(self) {
        loop {
            match self.listener.accept().await {
                Ok((stream, _)) => {
                    let new_dispatch = self.dispatch.clone();
                    tokio::spawn(async {
                        if let Err(err) = handle_client(stream, new_dispatch).await {
                            warn!("Failed to handle socket connection: {:?}", err);
                        }
                    });
                }
                Err(err) => {
                    warn!("Failed to create socket connection: {:?}", err);
                }
            };
        }
    }
}

/// Ensure parent directories
/// Remove potential lingering socket file from previous runs
fn prepare_path(socket_path: &Path) -> Result<()> {
    let parent = socket_path
        .parent()
        .context("socket file cannot be located directly under root")?;

    if !parent.exists() {
        info!("Creating socket directory: {:?}", &parent);
        let _ = std::fs::create_dir_all(parent);
    }

    // Not deleting the previous socket file results in a:
    // "Already in use" error
    if socket_path.exists() {
        debug!(
            "Previous socket file {:?} found, attempting to remove",
            socket_path
        );
        std::fs::remove_file(socket_path).context("failed to remove previous socket file")?;
    }

    Ok(())
}

async fn handle_client(mut stream: UnixStream, dispatch: DispatchChannels) -> Result<()> {
    use shared::types as t;
    info!("Got unix socket client: {:?}", stream);

    let mut request_message: String = String::new();
    stream.read_to_string(&mut request_message).await?;
    let message: t::ClientRequest = serde_json::from_str(&request_message)?;
    debug!("Got message from client: {:?}", &message);

    let response = handle_request(message, dispatch).await;
    let response_message = serde_json::to_string(&response)?;

    stream.write_all(response_message.as_bytes()).await?;
    stream.flush().await?;
    println!("Shutting down socket");
    stream.shutdown().await?;

    Ok(())
}

async fn handle_request(request: ClientRequest, dispatch: DispatchChannels) -> ClientResponse {
    use shared::types as t;
    use shared::types::ClientRequest as req;
    use shared::types::ClientResponse as resp;

    match request {
        req::Info => resp::Info(t::InfoResponse {
            status: t::ServerStatus::Active,
            version: "0.1.0".to_string(),
        }),
        req::Build(build_info) => {
            // TODO: we should not be doing this operation on the response thread
            // Instead, we should be sending a message for the evaluator service to traverse this
            dispatch
                .eval_sender
                .send(build_info.drv_path)
                .await
                .expect("Eval service is unhealthy");

            // TODO: We should likely return a URL to build status
            resp::Build(t::BuildResponse { enqueued: true })
        }
    }
}
