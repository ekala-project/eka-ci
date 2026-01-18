mod config;

use std::path::PathBuf;

use anyhow::{Context, Result};
use config::CIConfig;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::db::DbService;
use crate::github::{CICheckInfo, GitHubTask};
use crate::nix::{EvalJob, EvalTask};
use crate::services::AsyncService;

#[derive(Debug, Clone)]
pub enum RepoTask {
    Read(PathBuf),
    ReadGitHub {
        repo_path: PathBuf,
        ci_info: CICheckInfo,
    },
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
    repo_receiver: Option<mpsc::Receiver<RepoTask>>,
    eval_sender: mpsc::Sender<EvalTask>,
    github_sender: Option<mpsc::Sender<GitHubTask>>,
    db_service: DbService,
}

impl RepoReader {
    pub fn new(
        eval_sender: mpsc::Sender<EvalTask>,
        github_sender: Option<mpsc::Sender<GitHubTask>>,
        db_service: DbService,
    ) -> anyhow::Result<Self> {
        let (repo_sender, repo_receiver) = mpsc::channel(1000);

        Ok(Self {
            repo_sender,
            repo_receiver: Some(repo_receiver),
            eval_sender,
            github_sender,
            db_service,
        })
    }

    async fn process_github_repo_config(
        &self,
        mut path: PathBuf,
        ci_info: &CICheckInfo,
    ) -> anyhow::Result<()> {
        let root = path.clone();
        if let Ok(config) = read_repo_toplevel(&mut path) {
            debug!("Found CI Config: {:?}", &config);
            for (job_name, job) in config.jobs {
                if self
                    .db_service
                    .has_jobset(
                        &ci_info.commit,
                        &job_name,
                        &ci_info.owner,
                        &ci_info.repo_name,
                    )
                    .await?
                {
                    // We don't need to revisit jobs which already been processed
                    // For base commits, it's common that they could get processed multiple times
                    continue;
                }
                let file_path = resolve_file_path(root.clone(), path.clone(), job.file)?;
                let eval_job = EvalJob {
                    file_path: file_path.to_string_lossy().into(),
                    name: job_name,
                    allow_failures: job.allow_eval_failures,
                    push_command: job.push_command,
                };
                // TODO: Add jobset to db
                self.eval_sender
                    .send(EvalTask::GithubJobPR((eval_job, ci_info.clone())))
                    .await?;
            }
        } else {
            debug!("Repo was missing a CI config");
        }
        Ok(())
    }
}

impl AsyncService<RepoTask> for RepoReader {
    fn get_sender(&self) -> mpsc::Sender<RepoTask> {
        self.repo_sender.clone()
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<RepoTask>> {
        self.repo_receiver.take()
    }

    async fn handle_task(&self, task: RepoTask) -> anyhow::Result<()> {
        match task {
            // This is mostly for debugging, and evaluates a job free of any one PR
            RepoTask::Read(mut path) => {
                let root = path.clone();
                let config = read_repo_toplevel(&mut path)?;
                for (_job_name, job) in config.jobs {
                    let file_path = resolve_file_path(root.clone(), path.clone(), job.file)?;
                    let eval_job = EvalJob {
                        file_path: file_path.to_string_lossy().into(),
                        name: "local".to_string(),
                        allow_failures: job.allow_eval_failures,
                        push_command: job.push_command,
                    };
                    self.eval_sender.send(EvalTask::Job(eval_job)).await?;
                }
            },
            RepoTask::ReadGitHub { repo_path, ci_info } => {
                let configure_task = GitHubTask::CreateCIConfigureGate {
                    ci_check_info: ci_info.clone(),
                };
                let github_sender = self
                    .github_sender
                    .as_ref()
                    .context("GitHub app was not instantiated")?;
                github_sender.send(configure_task).await?;

                self.process_github_repo_config(repo_path, &ci_info).await?;

                let finish_configure_task = GitHubTask::CompleteCIConfigureGate {
                    ci_check_info: ci_info,
                };
                github_sender.send(finish_configure_task).await?;
            },
        }

        Ok(())
    }

    async fn handle_failure(&mut self, error: anyhow::Error) {
        warn!("Failure while handling repo action: {:?}", error);
    }

    async fn handle_closure(&mut self) {
        warn!("Closing repo service");
    }
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
