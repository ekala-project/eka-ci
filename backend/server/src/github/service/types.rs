use std::fmt;

use anyhow::Result;
use octocrab::Octocrab;
use octocrab::models::checks::CheckRun;
use octocrab::models::pulls::PullRequest;
use octocrab::params::checks::{CheckRunConclusion as GHConclusion, CheckRunStatus as GHStatus};

use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;
use crate::nix::nix_eval_jobs::NixEvalDrv;

#[derive(Debug, Clone)]
pub enum JobDifference {
    New,
    Changed,
    Removed,
}

impl fmt::Display for JobDifference {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            JobDifference::New => write!(f, "New"),
            JobDifference::Changed => write!(f, "Changed"),
            JobDifference::Removed => write!(f, "Removed"),
        }
    }
}

#[derive(Debug, Clone)]
/// Information needed to create a CI check run gate
pub struct CICheckInfo {
    pub commit: String,
    pub base_commit: Option<String>,
    pub owner: String,
    pub repo_name: String,
}

impl CICheckInfo {
    pub fn from_gh_pr_base(pr: &PullRequest) -> Self {
        let commit = pr.base.sha.clone();
        let repo = (*pr.base).repo.as_ref().unwrap();
        let owner = repo.owner.as_ref().unwrap().login.clone();
        let repo_name = repo.name.clone();

        Self {
            commit,
            base_commit: None,
            owner,
            repo_name,
        }
    }

    pub fn from_gh_pr_head(pr: &PullRequest) -> Self {
        let commit = pr.head.sha.clone();
        let base_commit = pr.base.sha.clone();
        let repo = (*pr.head).repo.as_ref().unwrap();
        let owner = repo.owner.as_ref().unwrap().login.clone();
        let repo_name = repo.name.clone();

        Self {
            commit,
            base_commit: Some(base_commit),
            owner,
            repo_name,
        }
    }

    pub async fn create_gh_check_run(
        &self,
        octocrab: &Octocrab,
        jobset_name: &str,
        name: &str,
        initial_status: DrvBuildState,
        difference: &JobDifference,
    ) -> Result<CheckRun> {
        let title = format!("{} / {} ({})", name, difference.to_string(), jobset_name);
        let (gh_status, gh_conclusion) = match difference {
            // If it's been removed, we don't really care what the previous status was
            JobDifference::Removed => (GHStatus::Completed, Some(GHConclusion::Neutral)),
            _ => initial_status.as_gh_checkrun_state(),
        };
        self.inner_gh_check_run(octocrab, &title, gh_status, gh_conclusion)
            .await
    }

    async fn inner_gh_check_run(
        &self,
        octocrab: &Octocrab,
        title: &str,
        gh_status: GHStatus,
        gh_conclusion: Option<GHConclusion>,
    ) -> Result<CheckRun> {
        let check_builder = octocrab.checks(&self.owner, &self.repo_name);
        let mut create_check_run = check_builder
            .create_check_run(title, &self.commit)
            .status(gh_status);

        if let Some(conclusion) = gh_conclusion {
            create_check_run = create_check_run.conclusion(conclusion);
        }

        let check_run = create_check_run.send().await?;
        Ok(check_run)
    }
}

#[derive(Debug)]
pub enum GitHubTask {
    UpdateBuildStatus {
        drv_id: DrvId,
        status: DrvBuildState,
    },
    CreateJobSet {
        ci_check_info: CICheckInfo,
        name: String,
        jobs: Vec<NixEvalDrv>,
    },
    CreateCIConfigureGate {
        ci_check_info: CICheckInfo,
    },
    CompleteCIConfigureGate {
        ci_check_info: CICheckInfo,
    },
    CreateCIEvalJob {
        ci_check_info: CICheckInfo,
        job_title: String,
    },
    CompleteCIEvalJob {
        ci_check_info: CICheckInfo,
    },
}

pub type Owner = String;
pub type Commit = String;
