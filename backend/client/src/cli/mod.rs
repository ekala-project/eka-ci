use std::path::PathBuf;

use clap::{Parser, Subcommand};
use shared::types as t;

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    /// Information about EkaCI running on host
    Info,
    /// Brief status and summary of EkaCI
    Status,
    /// Ask server to attempt to build a drv
    Build(t::BuildRequest),

    Job(t::JobRequest),

    Repo(t::RepoRequest),

    Git(t::GitRequest),

    Github(t::GitHubPrRequest),

    /// Inspect or Modify individual drvs
    #[command(subcommand)]
    Drv(DrvCommands),

    /// Query release channel status and promotion history
    #[command(subcommand)]
    Channel(ChannelCommands),
}

#[derive(Debug, Subcommand)]
pub(crate) enum DrvCommands {
    /// Request status for an individual Drv
    #[command(about)]
    Info(t::DrvStatusRequest),
}

#[derive(Debug, Subcommand)]
pub(crate) enum ChannelCommands {
    /// Query the current status and recent promotion history for a release channel
    #[command(about)]
    Status(t::ChannelStatusRequest),
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None, arg_required_else_help = true)]
pub(crate) struct Args {
    #[command(subcommand)]
    pub command: Option<Commands>,

    #[arg(short, long)]
    pub socket: Option<PathBuf>,
}
