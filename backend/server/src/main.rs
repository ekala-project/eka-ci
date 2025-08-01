mod client;
mod config;
mod db;
mod github;
mod nix;
mod scheduler;
mod web;

use crate::nix::EvalTask;
use anyhow::Context;
use client::UnixService;
use config::Config;
use tokio::{
    signal::unix::{signal, SignalKind},
    sync::mpsc::channel,
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, level_filters::LevelFilter, warn};
use tracing_subscriber::EnvFilter;
use web::WebService;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .with_ansi(true)
        .with_level(true)
        .with_target(true)
        .with_timer(tracing_subscriber::fmt::time())
        .init();

    let config = Config::from_env()?;
    debug!("Using configuration {config:?}");

    let db_service = db::DbService::new(&config.db_path)
        .await
        .context("attempted to create DB pool")?;

    let db_pool = db_service.pool.clone();

    let scheduler_service = scheduler::SchedulerService::new(db_service.clone())?;
    let (eval_sender, eval_receiver) = channel::<EvalTask>(1000);

    let eval_service = nix::EvalService::new(
        eval_receiver,
        db_service.clone(),
        scheduler_service.ingress_request_sender(),
    );

    let unix_service =
        UnixService::bind_to_path(&config.unix.socket_path, eval_sender, db_service.clone())
            .await
            .context("failed to start unix service")?;

    let web_service = WebService::bind_to_address(&config.web.address)
        .await
        .context("failed to start web service")?;

    if let Err(e) = github::register_app().await {
        // In dev environments, there usually is no authentication, but the server should still be
        // runnable. If someone however tried to configure authentication, make sure to tell them
        // load and clear if there was a problem.
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
    }

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

    let eval_handle = tokio::spawn(eval_service.run(cancellation_token.clone()));
    let unix_handle = tokio::spawn(unix_service.run(cancellation_token.clone()));
    let web_handle = tokio::spawn(web_service.run(cancellation_token.clone()));

    let mut sigterm = signal(SignalKind::terminate()).context("failed to get sigterm handle")?;
    let mut sigint = signal(SignalKind::interrupt()).context("failed to get sigint handle")?;

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
    _ = tokio::join!(eval_handle, unix_handle, web_handle);

    db_pool.close().await;

    info!("Database service pool closed");
    info!("All services shutdown gracefully");

    Ok(())
}
