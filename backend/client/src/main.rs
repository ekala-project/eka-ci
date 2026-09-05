mod api;
mod check;
mod cli;
mod requests;

use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use cli::{ChannelCommands, Commands, DrvCommands};
use requests::send_request;
use shared::dirs::eka_dirs;
use shared::types as t;
use shared::types::ClientRequest;
use tracing::debug;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::EnvFilter;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .with_ansi(true)
        .with_level(true)
        .with_target(true)
        .with_timer(tracing_subscriber::fmt::time())
        .init();

    let args = cli::Args::parse();

    // Resolve socket path lazily — only commands that use the unix socket need it.
    let resolve_socket = || -> anyhow::Result<PathBuf> {
        args.socket.clone().map_or_else(
            || {
                eka_dirs()?.get_runtime_file("ekaci.socket").context(
                    "failed to determine default path for unix socket, consider setting it \
                     explicitly",
                )
            },
            Result::Ok,
        )
    };

    match args.command {
        Some(Commands::Info) => {
            let socket = resolve_socket()?;
            send_request(&socket, ClientRequest::Info)
                .context("failed to send info request to server")?;
        },
        Some(Commands::Github(pr_info)) => {
            let socket = resolve_socket()?;
            send_request(&socket, ClientRequest::GitHub { pr: pr_info })
                .context("failed to send info request to server")?;
        },
        Some(Commands::Git(repo_info)) => {
            let socket = resolve_socket()?;
            send_request(&socket, ClientRequest::Git(repo_info))
                .context("failed to send info request to server")?;
        },
        Some(Commands::Status) => {},
        Some(Commands::Repo(repo_req)) => {
            let socket = resolve_socket()?;
            let file_path = PathBuf::from(repo_req.file_path).canonicalize()?;
            let request = t::RepoRequest {
                file_path: file_path.to_string_lossy().into(),
            };
            send_request(&socket, ClientRequest::Repo(request))
                .context("failed to send info request to server")?;
        },
        Some(Commands::Build(build_req)) => {
            let socket = resolve_socket()?;
            send_request(&socket, ClientRequest::Build(build_req))
                .context("failed to send info request to server")?;
        },
        Some(Commands::Drv(DrvCommands::Info(drv_status_request))) => {
            let socket = resolve_socket()?;
            send_request(&socket, ClientRequest::DrvStatus(drv_status_request))
                .context("failed to send info request to server")?;
        },
        Some(Commands::Drv(DrvCommands::Deps { drv_path })) => {
            let client = api::ApiClient::new(args.api);
            client
                .show_drv_deps(&drv_path)
                .await
                .context("failed to get derivation dependencies")?;
        },
        Some(Commands::Channel(ChannelCommands::Status(channel_status_request))) => {
            let socket = resolve_socket()?;
            send_request(
                &socket,
                ClientRequest::ChannelStatus(channel_status_request),
            )
            .context("failed to send channel status request to server")?;
        },
        Some(Commands::Job(req)) => {
            let socket = resolve_socket()?;
            let abs_file_path = std::fs::canonicalize(req.file_path)?
                .as_path()
                .to_str()
                .unwrap()
                .to_string();
            let abs_req = t::JobRequest {
                file_path: abs_file_path,
            };
            debug!("Requesting job eval: {:?}", &abs_req);
            send_request(&socket, ClientRequest::Job(abs_req))
                .context("failed to send info request to server")?;
        },
        Some(Commands::Check(check_cmd)) => {
            check::handle_check_command(check_cmd)
                .await
                .context("failed to execute check command")?;
        },
        Some(Commands::Pr {
            owner,
            repo,
            pr_number,
        }) => {
            let client = api::ApiClient::new(args.api);
            client
                .show_pr(&owner, &repo, pr_number)
                .await
                .context("failed to get PR status")?;
        },
        Some(Commands::Jobs { owner, repo }) => {
            let client = api::ApiClient::new(args.api);
            client
                .list_jobs(owner.as_deref(), repo.as_deref())
                .await
                .context("failed to list jobs")?;
        },
        Some(Commands::JobSet {
            jobset_id,
            state,
            failures,
        }) => {
            let client = api::ApiClient::new(args.api);
            client
                .show_jobset(jobset_id, state.as_deref(), failures)
                .await
                .context("failed to get jobset details")?;
        },
        Some(Commands::Log { drv_path }) => {
            let client = api::ApiClient::new(args.api);
            client
                .show_log(&drv_path)
                .await
                .context("failed to get build log")?;
        },
        Some(Commands::ResyncChecks { sha }) => {
            let socket = resolve_socket()?;
            let request = t::ResyncChecksRequest { sha };
            send_request(&socket, ClientRequest::ResyncChecks(request))
                .context("failed to send resync-checks request to server")?;
        },
        None => {},
    }

    Ok(())
}
