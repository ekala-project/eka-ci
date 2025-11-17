use anyhow::{Context, Result};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::{Sender, channel};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::ci::RepoReader;
use crate::client::UnixService;
use crate::config::Config;
use crate::db::DbService;
use crate::git::GitService;
use crate::github::{self, GitHubService};
use crate::nix::{EvalService, EvalTask};
use crate::scheduler::{IngressTask, SchedulerService};
use crate::web::WebService;

mod async_service;
pub use async_service::AsyncService;

pub async fn start_services(config: Config) -> Result<()> {
    let db_service = DbService::new(&config.db_path)
        .await
        .context("attempted to create DB pool")?;

    let db_pool = db_service.pool.clone();

    let scheduler_service =
        SchedulerService::new(db_service.clone(), config.remote_builders).await?;
    let (eval_sender, eval_receiver) = channel::<EvalTask>(1000);

    let maybe_github_service = match github::register_app().await {
        Ok(octocrab) => {
            let github_service = GitHubService::new(db_service.clone(), octocrab).await?;
            Some(github_service)
        },
        Err(e) => {
            // In dev environments, there usually is no authentication, but the server should still
            // be runnable. If someone however tried to configure authentication, make
            // sure to tell them load and clear if there was a problem.
            if matches!(e, github::AppRegistrationError::InvalidEnv(_)) {
                warn!(
                    "Skipping GitHub app registration: {}",
                    anyhow::Chain::new(&e)
                        .map(|e| e.to_string())
                        .collect::<Vec<_>>()
                        .join(": ")
                );
            } else {
                Err(e).context("failed to register GitHub app")?;
            }
            None
        },
    };

    let eval_service = EvalService::new(
        eval_receiver,
        db_service.clone(),
        scheduler_service.ingress_request_sender(),
        maybe_github_service.as_ref().map(|x| x.get_sender()),
    );

    let maybe_github_sender = maybe_github_service.as_ref().map(|x| x.get_sender());
    let repo_service = RepoReader::new(eval_sender.clone(), maybe_github_sender)?;
    let repo_sender = repo_service.get_sender();

    let git_service = GitService::new(repo_sender.clone())?;

    let web_service = WebService::bind_to_address(&config.web.address, git_service.get_sender())
        .await
        .context("failed to start web service")?;

    let unix_service = UnixService::bind_to_path(
        &config.unix.socket_path,
        eval_sender.clone(),
        repo_sender.clone(),
        db_service.clone(),
        git_service.get_sender(),
    )
    .await
    .context("failed to start unix service")?;

    // Use `bind_addr` instead of the `addr` + `port` given by the user, to ensure the printed
    // address is always correct (even for funny things like setting the port to 0).
    info!(
        "Serving Eka CI web service on http://{}",
        web_service.bind_addr(),
    );
    info!(
        "Listening for client connection on {}",
        unix_service
            .bind_addr()
            .as_pathname()
            .map_or("<<unnamed socket>>".to_owned(), |path| path
                .display()
                .to_string())
    );

    let cancellation_token = CancellationToken::new();

    let git_handle = tokio::spawn(git_service.run(cancellation_token.clone()));
    let repo_handle = tokio::spawn(repo_service.run(cancellation_token.clone()));
    let eval_handle = tokio::spawn(eval_service.run(cancellation_token.clone()));
    let unix_handle = tokio::spawn(unix_service.run(cancellation_token.clone()));
    let web_handle = tokio::spawn(web_service.run(cancellation_token.clone()));
    if let Some(github_service) = maybe_github_service {
        let _ = tokio::spawn(github_service.run(cancellation_token.clone()));
    }

    let mut sigterm = signal(SignalKind::terminate()).context("failed to get sigterm handle")?;
    let mut sigint = signal(SignalKind::interrupt()).context("failed to get sigint handle")?;

    enqueue_buildable_builds(
        db_service.clone(),
        scheduler_service.ingress_request_sender(),
    )
    .await?;

    tokio::select! {
        biased;
        _ = sigterm.recv() => {
            info!("Received SIGTERM, gracefully shutting down");
        }
        _ = sigint.recv() => {
            info!("Received SIGINT, gracefully shutting down");
        }
    }

    cancellation_token.cancel();

    // Wait for the services to shutdown
    _ = tokio::join!(
        eval_handle,
        unix_handle,
        web_handle,
        repo_handle,
        git_handle
    );

    db_pool.close().await;

    info!("Database service pool closed");
    info!("All services shutdown gracefully");

    Ok(())
}

async fn enqueue_buildable_builds(
    db_service: DbService,
    ingress_sender: Sender<IngressTask>,
) -> Result<()> {
    let queued_drvs = db_service.get_buildable_drvs().await?;

    info!("Checking {} drvs for build candidates", queued_drvs.len());
    for drv_id in queued_drvs {
        let ingress_task = IngressTask::CheckBuildable(drv_id);
        ingress_sender.send(ingress_task).await?;
    }

    Ok(())
}
