mod cli;

use clap::Parser;
use log::info;
use warp::Filter;
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

    let about = warp::path("about")
    .map(|| format!("About Page"));

    // TODO: serve up frontend bundle
    let root = warp::path::end()
    .map(|| format!("Welcome to Eka-CI"));

    let routes = warp::get().and(about.or(root));
    let addr = args.addr.parse::<Ipv4Addr>().expect("Invalid addr");

    info!(target: LOG_TARGET, "Serving Eka-CI on {}:{}", args.addr, args.port);

    warp::serve(routes)
    .run((addr, args.port))
    .await
}
