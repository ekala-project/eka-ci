#[cfg(feature = "bundle-proxy")]
use std::num::NonZeroU16;
use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub(crate) struct Args {
    #[arg(short, long)]
    #[arg(help = "Port for server to host http traffic")]
    #[clap(default_value_t = 3030 as u16)]
    pub port: u16,

    #[clap(default_value = "127.0.0.1")]
    #[arg(help = "IPv4 address to bind http traffic")]
    #[arg(short, long)]
    pub addr: String,

    #[arg(help = "Socket for ekaci client. Defaults to $XDG_RUNTIME_DIR/ekaci.")]
    #[arg(short, long)]
    pub socket: Option<PathBuf>,

    #[arg(help = "Path for the frontend bundle. Frontend will be disabled if not provided.")]
    #[arg(short, long)]
    pub bundle: Option<PathBuf>,

    /// The local port where all requests for the frontend should be forwarded to.
    ///
    /// This is a DEVELOPMENT TOOL. For a production deployment, use [bundle] instead!
    ///
    /// The main use case for this flag is to specify the port of a locally running hot reloading
    /// server, such as `live-server` or `elm-live`.
    #[cfg(feature = "bundle-proxy")]
    #[arg(long)]
    pub bundle_port: Option<NonZeroU16>,
}
