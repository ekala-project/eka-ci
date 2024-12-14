use clap::{Parser, Subcommand};

#[derive(Debug)]
#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Information about EkaCI running on host
    Info,
    /// Brief status and summary of EkaCI
    Status,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None, arg_required_else_help = true)]
pub(crate) struct Args {
    #[command(subcommand)]
    pub command: Option<Commands>,

    #[arg(short, long)]
    pub socket: Option<String>,
}

