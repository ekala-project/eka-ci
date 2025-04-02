mod cli;
mod error;
mod requests;

use crate::error::Result;
use chrono::Local;
use clap::Parser;
use cli::Commands;
use requests::send_request;
use shared::types::ClientRequest;
use std::io::Write;

fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .format(|buf, record| {
            writeln!(
                buf,
                "[{}] {:5} - {} - {}",
                Local::now().to_rfc3339(),
                record.level(),
                record.target(),
                record.args()
            )
        })
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
