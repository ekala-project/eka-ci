use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub(crate) struct Args {

    #[arg(short,long)]
    #[arg(help = "Port for server to host http traffic")]
    #[clap(default_value_t = 3030 as u16)]
    pub port: u16,

    #[clap(default_value = "127.0.0.1")]
    #[arg(help = "IPv4 address to bind http traffic")]
    #[arg(short,long)]
    pub addr: String,

    #[arg(help = "Socket for ekaci client. Defaults to $XDG_RUNTIME_DIR/ekaci.")]
    #[arg(short,long)]
    pub socket: Option<String>,
}
