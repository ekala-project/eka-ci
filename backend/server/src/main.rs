mod cli;
mod client;
mod db;
mod github;
mod error;
mod types;
mod web;

use clap::Parser;
use log::warn;
use chrono::Local;
use std::io::Write;
use std::net::Ipv4Addr;
use shared::dirs::eka_dirs;
use crate::error::{Result, LogResult};

#[tokio::main]
async fn main() -> Result<()> {
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

    std::fs::create_dir_all(eka_dirs().get_data_home());
    let db_file = eka_dirs().get_data_file("sqlite.db");
    db::initialize(&db_file.display().to_string()).await
        .log_with_context("attempted to create DB pool");

    let socket = args.socket.unwrap_or_else(||
        eka_dirs().get_runtime_file("ekaci.socket")
          .expect("failed to find xdg_runtime_dir after socket not set")
          .to_str()
          .expect("failed to make socket path into string")
          .to_string()
    );

    tokio::spawn(async { client::listen_for_client(socket).await });

    if let Err(e) = github::register_app().await {
        warn!("Failed to register as github app: {:?}", e);
    }

    let addr = args.addr.parse::<Ipv4Addr>().expect("Invalid addr");
    web::serve_web(addr, args.port).await;

    Ok(())
}
