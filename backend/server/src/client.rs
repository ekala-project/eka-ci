use crate::error::Result;
use log::{debug, info, warn};
use shared::types::{ClientRequest, ClientResponse};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{UnixListener, UnixStream}};
use std::path::Path;

/// Ensure parent directories
/// Remove potential lingering socket file from previous runs
fn prepare_path(socket: &str) {
    let socket_path = Path::new(&socket);
    let parent = socket_path.parent().expect("Socket can not be under root");

    if !parent.exists() {
        info!("Creating directory: {:?}", &parent);
        let _ = std::fs::create_dir_all(parent);
    }

    // Not deleting the previous socket file results in a:
    // "Already in use" error
    if socket_path.exists() {
        debug!("Previous socket file {:?} found, attempting to remove", socket_path);
        std::fs::remove_file(socket_path)
            .expect("Failed to remove previous socket");
    }
}

pub async fn listen_for_client(socket: String) -> Result<()> {
    prepare_path(&socket);

    // TODO: Remove previous socket if it exists
    info!("Attempting to listen on socket: {:?}", socket);
    let listener = UnixListener::bind(socket)?;

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

    let response = handle_request(message).await;
    let response_message = serde_json::to_string(&response)?;

    stream.write_all(response_message.as_bytes()).await?;
    stream.flush().await?;
    println!("Shutting down socket");
    stream.shutdown().await?;

    Ok(())
}

async fn handle_request(request: ClientRequest) -> ClientResponse {
    use shared::types::ClientRequest as req;
    use shared::types::ClientResponse as resp;
    use shared::types as t;

    match request {
        req::Info => resp::Info (t::InfoResponse {
            status: t::ServerStatus::Active,
            version : (env!("CARGO_PKG_VERSION")).to_string(),
        }),
        req::EvalPR(eval_info) => {
            debug!("Eval Request received: {:?}", &eval_info);
            let pr_info = crate::github::github_pull(&eval_info.domain, &eval_info.owner, &eval_info.repo, eval_info.number).await;
            debug!("Got info {:?}", &pr_info);
            resp::EvalPR (t::EvalPRResponse {
                // TODO: actually schedule reponse
                eval_id: 1_u32,
            })
        },
    }
}
