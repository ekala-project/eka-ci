mod config;
mod cli;
mod client;
mod github;
mod web;

use anyhow::Context;
use clap::Parser;
use client::UnixService;
use config::Config;
use tracing::{info, level_filters::LevelFilter, warn};
use tracing_subscriber::EnvFilter;
use web::WebService;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dbg!(Config::from_env()?);
    return Ok(());

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
