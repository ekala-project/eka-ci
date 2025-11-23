use anyhow::Result;
use octocrab::Octocrab;
use octocrab::models::checks::CheckRun;
use octocrab::models::pulls::PullRequest;

use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;
use crate::nix::nix_eval_jobs::NixEvalDrv;

#[derive(Debug, Clone)]
/// Information needed to create a CI check run gate
pub struct CICheckInfo {
    pub commit: String,
    pub base_commit: String,
    pub owner: String,
    pub repo_name: String,
}

impl CICheckInfo {
    pub fn from_gh_pr(pr: &PullRequest) -> Self {
        let commit = pr.head.sha.clone();
        let base_commit = pr.base.sha.clone();
        let repo = (*pr.head).repo.as_ref().unwrap();
        let owner = repo.owner.as_ref().unwrap().login.clone();
        let repo_name = repo.name.clone();

        Self {
            commit,
            base_commit,
            owner,
            repo_name,
        }
    }

    pub async fn create_gh_check_run(
        &self,
        octocrab: &Octocrab,
        name: &str,
        initial_status: DrvBuildState,
    ) -> Result<CheckRun> {
        let (gh_status, gh_conclusion) = initial_status.as_gh_checkrun_state();

        let check_builder = octocrab.checks(&self.owner, &self.repo_name);
        let mut create_check_run = check_builder
            .create_check_run(&format!("{} / Changed Drv", name), &self.commit)
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
}

pub type Owner = String;
pub type Commit = String;
