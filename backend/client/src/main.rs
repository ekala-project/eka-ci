mod cli;
mod error;
mod requests;

use crate::error::Result;
use clap::Parser;
use cli::Commands;
use requests::send_request;
use shared::types::ClientRequest;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
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
            send_request(args, ClientRequest::Info)?;
        }
        Some(Commands::Status) => {}
        None => {}
    }

    Ok(())
}
