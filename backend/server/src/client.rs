use std::path::Path;

use anyhow::{Context, Result};
use octocrab::Octocrab;
use shared::types::{ClientRequest, ClientResponse, DrvStatusResponse};
use tokio::io::AsyncWriteExt;
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
use crate::scheduler::IngressTask;

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
    ingress_sender: Sender<IngressTask>,
    db_service: DbService,
}

impl UnixService {
    pub async fn bind_to_path(
        socket_path: &Path,
        eval_sender: Sender<EvalTask>,
        repo_sender: Sender<RepoTask>,
        db_service: DbService,
        git_sender: Sender<GitTask>,
        ingress_sender: Sender<IngressTask>,
    ) -> Result<Self> {
        prepare_path(socket_path)?;

        let listener = UnixListener::bind(socket_path)?;
        let dispatch = DispatchChannels {
            eval_sender,
            repo_sender,
            git_sender,
            ingress_sender,
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
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create socket parent directory {:?}", parent))?;
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

async fn handle_client(stream: UnixStream, dispatch: DispatchChannels) -> Result<()> {
    use shared::types as t;
    use tokio::io::{AsyncBufReadExt, BufReader};

    info!("Got unix socket client: {:?}", stream);

    // Use newline-delimited JSON protocol
    // Wrap stream in BufReader to efficiently read line-by-line
    let mut reader = BufReader::new(stream);
    let mut request_message = String::new();
    reader.read_line(&mut request_message).await?;

    let message: t::ClientRequest = serde_json::from_str(&request_message)?;
    debug!("Got message from client: {:?}", &message);

    let response = handle_request(message, dispatch).await;
    let response_message = serde_json::to_string(&response)?;

    // Get the underlying stream back to write response
    let mut stream = reader.into_inner();
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
                config_json: None, // No config for client-initiated jobs
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
            use std::str::FromStr;

            use crate::db::model::drv_id;
            use crate::scheduler::IngressTask;

            // Check if this is a rebuild request
            if build_info.force {
                if build_info.rebuild_all {
                    // Rebuild all failed drvs
                    let task = IngressTask::RebuildAllFailed;
                    dispatch
                        .ingress_sender
                        .send(task)
                        .await
                        .expect("Ingress service is unhealthy");
                } else {
                    // Rebuild specific drv
                    if let Ok(drv_id) = drv_id::DrvId::from_str(&build_info.drv_path) {
                        let task = IngressTask::RebuildFailed(std::sync::Arc::new(drv_id));
                        dispatch
                            .ingress_sender
                            .send(task)
                            .await
                            .expect("Ingress service is unhealthy");
                    }
                }
            } else {
                // Normal build flow
                let task = EvalTask::TraverseDrv(build_info.drv_path);
                dispatch
                    .eval_sender
                    .send(task)
                    .await
                    .expect("Eval service is unhealthy");
            }

            resp::Ack(true)
        },
        req::DrvStatus(drv_status_request) => {
            use std::str::FromStr;

            use crate::db::model::build_event::DrvBuildState;
            use crate::db::model::drv_id;

            // Parse the drv_id from the request
            let drv_id = match drv_id::DrvId::from_str(&drv_status_request.drv_path) {
                Ok(id) => id,
                Err(e) => {
                    return resp::DrvStatus(Err(format!(
                        "Invalid derivation path '{}': {}",
                        drv_status_request.drv_path, e
                    )));
                },
            };

            // Query the database for the derivation
            let maybe_drv = match dispatch.db_service.get_drv(&drv_id).await {
                Ok(drv) => drv,
                Err(e) => {
                    return resp::DrvStatus(Err(format!(
                        "Database error querying derivation: {}",
                        e
                    )));
                },
            };

            // Return the derivation status or indicate it hasn't been seen
            match maybe_drv {
                Some(drv) => {
                    // Query failed dependencies if in TransitiveFailure state
                    let failed_deps = if matches!(drv.build_state, DrvBuildState::TransitiveFailure)
                    {
                        dispatch
                            .db_service
                            .get_failed_dependencies(&drv_id)
                            .await
                            .ok()
                            .map(|deps| deps.into_iter().map(|d| d.store_path()).collect())
                    } else {
                        None
                    };

                    resp::DrvStatus(Ok(DrvStatusResponse {
                        drv_path: drv.drv_path.store_path(),
                        status: format!("{:?}", drv.build_state),
                        failed_dependencies: failed_deps,
                    }))
                },
                None => resp::DrvStatus(Err(format!(
                    "Derivation '{}' has not been encountered by the system",
                    drv_status_request.drv_path
                ))),
            }
        },
        req::ChannelStatus(channel_status_request) => {
            use shared::types::{ChannelPromotion, ChannelStatusResponse};

            // Query the database for channel promotions
            let channel_id = format!("github:*/*:{}", channel_status_request.channel_name);

            // Get in-flight evaluation (if any)
            let in_flight = match crate::db::channels::get_in_flight(&channel_id, &dispatch.db_service.pool).await {
                Ok(Some(row)) => {
                    // Convert unix timestamp to human-readable date
                    use chrono::{TimeZone, Utc};
                    let dt = Utc.timestamp_opt(row.started_at, 0).unwrap();

                    Some(ChannelPromotion {
                        tracking_sha: row.tracking_sha,
                        target_branch: row.target_branch,
                        status: row.status.to_string(),
                        created_at: dt.to_rfc3339(),
                        blocked_reason: row.blocked_reason,
                    })
                },
                Ok(None) => None,
                Err(e) => {
                    return resp::ChannelStatus(Err(format!(
                        "Database error querying in-flight evaluation: {}",
                        e
                    )));
                },
            };

            // Get recent promotions (last 10)
            let recent_rows = match sqlx::query_as::<_, (String, String, i64, i64, Option<String>)>(
                "SELECT tracking_sha, target_branch, status, started_at, blocked_reason
                 FROM ChannelPromotion
                 WHERE channel_id LIKE ?
                 ORDER BY started_at DESC
                 LIMIT 10"
            )
            .bind(format!("github:%:{}", channel_status_request.channel_name))
            .fetch_all(&dispatch.db_service.pool)
            .await
            {
                Ok(rows) => rows,
                Err(e) => {
                    return resp::ChannelStatus(Err(format!(
                        "Database error querying recent promotions: {}",
                        e
                    )));
                },
            };

            let recent_promotions = recent_rows
                .into_iter()
                .map(|(sha, branch, status, started, blocked)| {
                    use chrono::{TimeZone, Utc};
                    let dt = Utc.timestamp_opt(started, 0).unwrap();

                    ChannelPromotion {
                        tracking_sha: sha,
                        target_branch: branch,
                        status: status.to_string(),
                        created_at: dt.to_rfc3339(),
                        blocked_reason: blocked,
                    }
                })
                .collect();

            resp::ChannelStatus(Ok(ChannelStatusResponse {
                channel_id: channel_id.clone(),
                in_flight,
                recent_promotions,
            }))
        },
    }
}
