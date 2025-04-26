mod cli;
mod requests;

use anyhow::Context;
use clap::Parser;
use cli::Commands;
use requests::send_request;
use shared::dirs::eka_dirs;
use shared::types as t;
use shared::types::ClientRequest;
use tracing::debug;
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
    let socket = args.socket.map_or_else(
        || {
            eka_dirs().get_runtime_file("ekaci.socket").context(
                "failed to determine default path for unix socket, consider setting it explicitly",
            )
        },
        Result::Ok,
    )?;

    match args.command {
        Some(Commands::Info) => {
            send_request(&socket, ClientRequest::Info)
                .context("failed to send info request to server")?;
        }
        Some(Commands::Status) => {}
        Some(Commands::Build(build_req)) => {
            send_request(&socket, ClientRequest::Build(build_req))
                .context("failed to send info request to server")?;
        }
        Some(Commands::Job(req)) => {
            let abs_file_path = std::fs::canonicalize(req.file_path)?
                .as_path()
                .to_str()
                .unwrap()
                .to_string();
            let abs_req = t::JobRequest {
                file_path: abs_file_path,
            };
            debug!("Requesting job eval: {:?}", &abs_req);
            send_request(&socket, ClientRequest::Job(abs_req))
                .context("failed to send info request to server")?;
        }
        None => {}
    }

    Ok(())
}
