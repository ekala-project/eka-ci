mod cli;
mod client;
mod error;
mod github;
mod web;

use crate::error::Result;
use clap::Parser;
use shared::dirs::eka_dirs;
use std::net::Ipv4Addr;
use tracing::{info, level_filters::LevelFilter, warn};
use tracing_subscriber::EnvFilter;

const LOG_TARGET: &str = "eka-ci::server::main";

#[tokio::main]
async fn main() -> Result<()> {
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

    let socket = args.socket.unwrap_or_else(|| {
        eka_dirs()
            .get_runtime_file("ekaci.socket")
            .expect("failed to find xdg_runtime_dir after socket not set")
            .to_str()
            .expect("failed to make socket path into string")
            .to_string()
    });

    tokio::spawn(async { client::listen_for_client(socket).await });

    if let Err(e) = github::register_app().await {
        warn!(target: &LOG_TARGET, "Failed to register as github app: {:?}", e);
    }

    let addr = args.addr.parse::<Ipv4Addr>().expect("Invalid addr");
    let listener = tokio::net::TcpListener::bind((addr, args.port)).await?;
    info!(
        "Serving Eka CI web service on http://{}",
        listener.local_addr()?,
    );
    web::serve_web(listener, args.bundle).await;

    Ok(())
}
