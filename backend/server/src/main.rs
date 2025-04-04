mod cli;
mod client;
mod error;
mod github;
mod web;

use anyhow::Context;
use clap::Parser;
use client::UnixService;
use tracing::{info, level_filters::LevelFilter, warn};
use tracing_subscriber::EnvFilter;
use web::WebService;

const LOG_TARGET: &str = "eka-ci::server::main";

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

    let args = cli::Args::parse();

    let unix_servie = UnixService::bind_to_path_or_default(args.socket)
        .await
        .context("failed to start unix service")?;
    let web_service = WebService::bind_to_addr_and_port(args.addr, args.port)
        .await
        .context("failed to start web service")?;

    if let Err(e) = github::register_app().await {
        warn!(target: &LOG_TARGET, "Failed to register as github app: {:?}", e);
    }

    // Use `bind_addr` instead of the `addr` + `port` given by the user, to ensure the printed
    // address is always correct (even for funny things like setting the port to 0).
    info!(
        "Serving Eka CI web service on http://{}",
        web_service.bind_addr(),
    );
    info!(
        "Listening for client connection on {}",
        unix_servie
            .bind_addr()
            .as_pathname()
            .map_or("<<unnamed socket>>".to_owned(), |path| path
                .display()
                .to_string())
    );

    tokio::spawn(async { unix_servie.run().await });
    web_service.run(args.bundle).await;

    Ok(())
}
