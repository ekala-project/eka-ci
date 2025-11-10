mod config;

use std::path::PathBuf;

use anyhow::Result;
use config::CIConfig;
use octocrab::models::pulls::PullRequest;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::nix::{EvalJob, EvalTask};

#[derive(Debug, Clone)]
pub enum RepoTask {
    Read(PathBuf),
    ReadPR((PathBuf, PullRequest)),
}
/// This service will receive a repo checkout and determine what CI jobs need
/// to be ran.
///
/// In particular, this involves reading the content of the .ekaci/ directory,
/// and determining if there's legacy CI jobs or flake outputs
#[allow(dead_code)]
pub struct RepoReader {
    // Channels to individual services
    // We may in the future need to recover an individual service, so retaining
    // a handle to the other service channels will be prequisite
    repo_sender: mpsc::Sender<RepoTask>,
    repo_receiver: mpsc::Receiver<RepoTask>,
}

impl RepoReader {
    pub fn new() -> anyhow::Result<Self> {
        let (repo_sender, repo_receiver) = mpsc::channel(1000);

        Ok(Self {
            repo_sender,
            repo_receiver,
        })
    }

    /// Producer channel for sending build requests for eventual scheduling
    pub fn repo_request_sender(&self) -> mpsc::Sender<RepoTask> {
        self.repo_sender.clone()
    }

    pub fn run(
        self,
        eval_sender: mpsc::Sender<EvalTask>,
        cancel_token: CancellationToken,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            cancel_token
                .run_until_cancelled(repo_tasks_loop(self.repo_receiver, eval_sender))
                .await;
        })
    }
}

async fn repo_tasks_loop(
    mut receiver: mpsc::Receiver<RepoTask>,
    eval_sender: mpsc::Sender<EvalTask>,
) {
    loop {
        if let Some(task) = receiver.recv().await {
            debug!("Received repo task {:?}", &task);
            if let Err(e) = handle_repo_task(task.clone(), &eval_sender).await {
                warn!("Failed to handle ingress request {:?}: {:?}", &task, e);
            }
        }
    }
}

async fn handle_repo_task(
    task: RepoTask,
    eval_sender: &mpsc::Sender<EvalTask>,
) -> anyhow::Result<()> {
    match task {
        // This is mostly for debugging, and evaluates a job free of any one PR
        RepoTask::Read(mut path) => {
            let root = path.clone();
            let config = read_repo_toplevel(&mut path)?;
            for (_job_name, job) in config.jobs {
                let file_path = resolve_file_path(root.clone(), path.clone(), job.file)?;
                let eval_job = EvalJob {
                    file_path: file_path.to_string_lossy().into(),
                };
                eval_sender.send(EvalTask::Job(eval_job)).await?;
            }
        },
        RepoTask::ReadPR((mut path, pr)) => {
            let root = path.clone();
            let config = read_repo_toplevel(&mut path)?;
            for (_job_name, job) in config.jobs {
                let file_path = resolve_file_path(root.clone(), path.clone(), job.file)?;
                let eval_job = EvalJob {
                    file_path: file_path.to_string_lossy().into(),
                };
                // TODO: Add jobset to db
                eval_sender
                    .send(EvalTask::GithubJobPR((eval_job, pr.clone())))
                    .await?;
            }
        },
    }

    Ok(())
}

fn read_repo_toplevel(path: &mut PathBuf) -> Result<CIConfig> {
    path.push(".ekaci");
    path.push("config.json");

    debug!("Received ask to read path: {:?}", &path);
    if !path.exists() {
        anyhow::bail!("No CI config located at {:?}, skipping", &path);
    }

    let contents = std::fs::read_to_string(&path)?;
    let config = CIConfig::from_str(&contents)?;
    Ok(config)
}

/// Resolve the file path
/// Absolute file paths will be traversed from repo root
/// Relative paths are traversed from .ekaci directory
fn resolve_file_path(
    mut repo_root: PathBuf,
    mut _file_path_to_config: PathBuf,
    file_path_in_config: PathBuf,
) -> anyhow::Result<PathBuf> {
    let file_path = if file_path_in_config.is_absolute() {
        // pushing an absolute path replaces the old value, we must:
        //   stringify the value
        //   remove leading "/"
        //   and then repush the "relative" directory from root
        let file_part: String = file_path_in_config.to_string_lossy().into();
        let file_part = file_part.strip_prefix("/").unwrap();
        repo_root.push(file_part);
        repo_root
    } else {
        // This is blocked on `normalize_lexically` being stabilized
        // https://github.com/rust-lang/rust/issues/134694
        //
        // file_path_to_config.pop();
        // file_path_to_config.push(file_path_in_config);
        // file_path_to_config
        anyhow::bail!("File paths must be absolute");
    };
    Ok(file_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO: blocked until normalize_lexically is stabilized
    // #[test]
    // fn test_relative_file_resolve() {
    //     let repo_root = PathBuf::from("/foo");
    //     let file_path_to_config = PathBuf::from("/foo/.ekaci/config.json");
    //     let file_path_in_config = PathBuf::from("../foo.nix");

    //     let resolved_path = resolve_file_path(repo_root, file_path_to_config,
    // file_path_in_config).unwrap();     assert_eq!(resolved_path,
    // PathBuf::from("/foo/foo.nix")); }

    #[test]
    fn test_absolute_file_resolve() {
        let repo_root = PathBuf::from("/foo");
        let file_path_to_config = PathBuf::from("/foo/.ekaci/config.json");
        let file_path_in_config = PathBuf::from("/foo.nix");

        let resolved_path =
            resolve_file_path(repo_root, file_path_to_config, file_path_in_config).unwrap();
        assert_eq!(resolved_path, PathBuf::from("/foo/foo.nix"));
    }
}
