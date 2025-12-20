use std::path::Path;

use anyhow::{Context, Result};
use octocrab::Octocrab;
use shared::types::{ClientRequest, ClientResponse, DrvStatusResponse};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::SocketAddr;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::Sender;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::ci::RepoTask;
use crate::db::DbService;
use crate::git::{GitTask, GitWorkspace};
use crate::nix::EvalTask;

pub struct UnixService {
    listener: UnixListener,
    /// Channel to emit drvs to be evaluated
    dispatch: DispatchChannels,
}

/// Channels which can be used to communicate actions to other services
#[derive(Clone)]
struct DispatchChannels {
    eval_sender: Sender<EvalTask>,
    repo_sender: Sender<RepoTask>,
    git_sender: Sender<GitTask>,
    db_service: DbService,
}

impl UnixService {
    pub async fn bind_to_path(
        socket_path: &Path,
        eval_sender: Sender<EvalTask>,
        repo_sender: Sender<RepoTask>,
        db_service: DbService,
        git_sender: Sender<GitTask>,
    ) -> Result<Self> {
        prepare_path(socket_path)?;

        let listener = UnixListener::bind(socket_path)?;
        let dispatch = DispatchChannels {
            eval_sender,
            repo_sender,
            git_sender,
            db_service,
        };

        Ok(Self { listener, dispatch })
    }

    pub fn bind_addr(&self) -> SocketAddr {
        // If the call fails either the system ran out of resources or libc is broken, for both of
        // these cases a panic seems appropiate.
        self.listener
            .local_addr()
            .expect("getsockname should always succeed on a properly initialized listener")
    }

    pub async fn run(self, cancellation_token: CancellationToken) {
        let mut join_set = JoinSet::new();

        while let Some(request) = cancellation_token
            .run_until_cancelled(self.listener.accept())
            .await
        {
            let stream = match request {
                Ok((stream, _)) => stream,
                Err(e) => {
                    error!(error = %e, "Failed to create socket connection");

                    use std::io::ErrorKind::*;
                    if !matches!(
                        e.kind(),
                        ConnectionReset | ConnectionAborted | BrokenPipe | TimedOut
                    ) {
                        warn!("Error was irrecoverable, shutting down");
                        cancellation_token.cancel();
                        break;
                    }

                    continue;
                },
            };

            let new_dispatch = self.dispatch.clone();
            join_set.spawn(async {
                if let Err(e) = handle_client(stream, new_dispatch).await {
                    error!(error = %e, "Failed to handle socket connection");
                }
            });
        }

        while join_set.join_next().await.is_some() {
            debug!("Client task completed during shutdown");
        }

        info!("Unix service shutdown gracefully")
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
    info!("Shutting down socket");
    stream.shutdown().await?;

    Ok(())
}

async fn handle_request(request: ClientRequest, dispatch: DispatchChannels) -> ClientResponse {
    use shared::types as t;
    use shared::types::{ClientRequest as req, ClientResponse as resp};

    match request {
        req::Info => resp::Info(t::InfoResponse {
            status: t::ServerStatus::Active,
            version: "0.1.0".to_string(),
        }),
        req::GitHub { pr } => {
            let octocrab = Octocrab::builder()
                .build()
                .expect("failed to construct octocrab");
            match octocrab.pulls(&pr.owner, &pr.repo).get(pr.pr).await {
                Ok(github_pr) => {
                    let task = GitTask::GitHubCheckout(github_pr);
                    dispatch
                        .git_sender
                        .send(task)
                        .await
                        .expect("Failed to send github task");
                    resp::Ack(true)
                },
                Err(e) => {
                    error!(
                        "Failed to fetch PR {}/{}/pull/{}: {}",
                        pr.owner, pr.repo, pr.pr, e
                    );
                    resp::Ack(false)
                },
            }
        },
        req::Git(git_info) => {
            let task = GitTask::Checkout(GitWorkspace::from_git_request(git_info));
            dispatch
                .git_sender
                .send(task)
                .await
                .expect("Failed to send git task");
            resp::Ack(true)
        },
        req::Repo(repo_info) => {
            let repo_request = RepoTask::Read(repo_info.file_path.into());
            dispatch
                .repo_sender
                .send(repo_request)
                .await
                .expect("Failed to send repo task");
            resp::Ack(true)
        },
        req::Job(job_info) => {
            let job = crate::nix::EvalJob {
                file_path: job_info.file_path,
                name: "client".to_string(),
                allow_failures: true,
            };
            let task = EvalTask::Job(job);
            dispatch
                .eval_sender
                .send(task)
                .await
                .expect("Eval service is unhealthy");

            resp::Ack(true)
        },
        req::Build(build_info) => {
            let task = EvalTask::TraverseDrv(build_info.drv_path);
            dispatch
                .eval_sender
                .send(task)
                .await
                .expect("Eval service is unhealthy");

            resp::Ack(true)
        },
        req::DrvStatus(drv_status_request) => {
            use std::str::FromStr;

            use crate::db::model::drv_id;

            if let Ok(drv_id) = drv_id::DrvId::from_str(&drv_status_request.drv_path) {
                let maybe_drv = dispatch.db_service.get_drv(&drv_id).await.unwrap();
                let inner = maybe_drv.map(|x| DrvStatusResponse {
                    drv_path: x.drv_path.store_path(),
                    status: format!("{:?}", x.build_state),
                });
                return resp::DrvStatus(inner);
            }

            // TODO: Send actual error
            resp::DrvStatus(None)
        },
    }
}
