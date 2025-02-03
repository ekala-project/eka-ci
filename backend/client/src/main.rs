mod cli;
mod error;
mod requests;

use chrono::Local;
use clap::Parser;
use cli::Commands;
use requests::send_request;
use shared::types::ClientRequest;
use std::io::Write;
use crate::error::Result;

fn main() -> Result <()> {
    env_logger::Builder::from_default_env()
        .format(|buf, record| {
            writeln!(buf,
                "[{}] {:5} - {} - {}",
                Local::now().to_rfc3339(),
                record.level(),
                record.target(),
                record.args()
            )
        }).init();

    let args = cli::Args::parse();
    let socket = requests::socket_addr(args.socket);

    match args.command {
        Some(Commands::Info) => {
            send_request(socket, ClientRequest::Info)?;
        },
        Some(Commands::EvalPR(eval_args)) => {
            let request = ClientRequest::EvalPR(eval_args);
            send_request(socket, request)?;
        },
        Some(Commands::Status) => {
        },
        None => {
        },
    }

    Ok(())
}
