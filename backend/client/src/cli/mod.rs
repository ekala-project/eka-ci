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

    /// Run checks locally with CI-equivalent environment
    #[command(subcommand)]
    Check(crate::check::CheckCommand),

    /// Show PR build status and progress
    Pr {
        /// Repository owner
        owner: String,
        /// Repository name
        repo: String,
        /// Pull request number
        pr_number: i64,
    },

    /// List active jobsets with build progress
    Jobs {
        /// Filter by repository owner
        #[arg(long)]
        owner: Option<String>,
        /// Filter by repository name
        #[arg(long)]
        repo: Option<String>,
    },

    /// Show jobset details and derivation status
    #[command(name = "jobset")]
    JobSet {
        /// Jobset ID to inspect
        jobset_id: i64,
        /// Filter derivations by build state
        #[arg(long)]
        state: Option<String>,
        /// Show only failed/blocked derivations
        #[arg(long)]
        failures: bool,
    },

    /// Show build log for a derivation
    Log {
        /// Derivation path (hash-name.drv or /nix/store/...)
        drv_path: String,
    },

    /// Re-sync check run states to GitHub for a commit
    ResyncChecks {
        /// Git commit SHA
        sha: String,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum DrvCommands {
    /// Request status for an individual Drv
    #[command(about)]
    Info(t::DrvStatusRequest),

    /// Show derivation dependencies and their build states
    Deps {
        /// Derivation path (hash-name.drv or /nix/store/...)
        drv_path: String,
    },
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

    /// Base URL of the EkaCI web API
    #[arg(long, default_value = "http://127.0.0.1:3030")]
    pub api: String,
}
