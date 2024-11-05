mod cli;
mod github;
mod error;
mod web;

use clap::Parser;
use log::warn;
use chrono::Local;
use std::io::Write;
use std::net::Ipv4Addr;

const LOG_TARGET: &str = "eka-ci::server::main";

#[tokio::main]
async fn main() {
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

    match github::register_app().await {
        Err(e) => warn!(target: &LOG_TARGET, "Failed to register as github app: {:?}", e),
        _ => { },
    }

    let addr = args.addr.parse::<Ipv4Addr>().expect("Invalid addr");

    web::serve_web(addr, args.port).await
}
