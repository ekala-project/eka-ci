use anyhow::{Context, Result};
use shared::types::{ClientRequest, ClientResponse};
use std::path::Path;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{unix::SocketAddr, UnixListener, UnixStream},
};
use tracing::{debug, info, warn};

pub struct UnixService {
    listener: UnixListener,
}

impl UnixService {
    pub async fn bind_to_path(socket_path: &Path) -> Result<Self> {
        prepare_path(socket_path)?;

        let listener = UnixListener::bind(socket_path)?;

        Ok(Self { listener })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        // If the call fails either the system ran out of resources or libc is broken, for both of
        // these cases a panic seems appropiate.
        self.listener
            .local_addr()
            .expect("getsockname should always succeed on a properly initialized listener")
    }

    pub async fn run(self) {
        listen_for_client(self.listener).await
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

async fn listen_for_client(listener: UnixListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(async {
                    if let Err(err) = handle_client(stream).await {
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

async fn handle_client(mut stream: UnixStream) -> Result<()> {
    use shared::types as t;
    info!("Got unix socket client: {:?}", stream);

    let mut request_message: String = String::new();
    stream.read_to_string(&mut request_message).await?;
    let message: t::ClientRequest = serde_json::from_str(&request_message)?;
    debug!("Got message from client: {:?}", &message);

    let response = handle_request(message);
    let response_message = serde_json::to_string(&response)?;

    stream.write_all(response_message.as_bytes()).await?;
    stream.flush().await?;
    println!("Shutting down socket");
    stream.shutdown().await?;

    Ok(())
}

fn handle_request(request: ClientRequest) -> ClientResponse {
    use shared::types as t;
    use shared::types::ClientRequest as req;
    use shared::types::ClientResponse as resp;

    match request {
        req::Info => resp::Info(t::InfoResponse {
            status: t::ServerStatus::Active,
            version: "0.1.0".to_string(),
        }),
    }
}
