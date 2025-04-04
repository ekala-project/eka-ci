mod cli;
mod requests;

use anyhow::Context;
use clap::Parser;
use cli::Commands;
use requests::send_request;
use shared::types::ClientRequest;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
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

    match &args.command {
        Some(Commands::Info) => {
            send_request(args, ClientRequest::Info)
                .context("failed to send info request to server")?;
        }
        Some(Commands::Status) => {}
        None => {}
    }

    Ok(())
}
