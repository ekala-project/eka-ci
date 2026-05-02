// Check run management

use anyhow::Result;
use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};
use tracing::warn;

use super::GitHubService;
use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;
use crate::github::service::{CICheckInfo, JobDifference, actions};

impl GitHubService {
    pub(super) async fn handle_cancel_check_runs_for_commit(
        &self,
        ci_check_info: &CICheckInfo,
    ) -> Result<()> {
        let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

        // Cancel any in-progress configure gate.
        if let Some(check_run_id) = self
            .github_configure_checks
            .lock()
            .await
            .remove(&ci_check_info.commit)
        {
            if let Err(e) = actions::update_ci_configure_gate(
                &octocrab,
                ci_check_info,
                check_run_id,
                CheckRunStatus::Completed,
                CheckRunConclusion::Cancelled,
            )
            .await
            {
                warn!(
                    "Failed to cancel configure gate for {}: {:?}",
                    &ci_check_info.commit, e
                );
            }
        }

        // Cancel in-progress eval gates (may be multiple per commit).
        let keys_to_remove: Vec<_> = self
            .github_eval_checks
            .lock()
            .await
            .keys()
            .filter(|(commit, _)| commit == &ci_check_info.commit)
            .cloned()
            .collect();

        for key in keys_to_remove {
            if let Some(check_run_id) = self.github_eval_checks.lock().await.remove(&key) {
                if let Err(e) = actions::update_ci_eval_job(
                    &octocrab,
                    ci_check_info,
                    check_run_id,
                    CheckRunStatus::Completed,
                    CheckRunConclusion::Cancelled,
                )
                .await
                {
                    warn!(
                        "Failed to cancel eval gate for {}: {:?}",
                        &ci_check_info.commit, e
                    );
                }
            }
        }

        // Cancel all job check_runs for this commit.
        let check_runs = self
            .db_service
            .check_runs_for_commit(&ci_check_info.commit)
            .await?;
        for check_run in check_runs {
            if let Err(e) = check_run
                .send_gh_update(
                    &octocrab,
                    &DrvBuildState::Interrupted(
                        crate::db::model::build_event::DrvBuildInterruptionKind::Cancelled,
                    ),
                )
                .await
            {
                warn!(
                    "Failed to cancel check_run {} for {}: {:?}",
                    check_run.check_run_id, &ci_check_info.commit, e
                );
            }
        }
        Ok(())
    }

    pub(super) async fn handle_create_failure_check_run(
        &self,
        drv_id: &DrvId,
        jobset_id: i64,
        job_attr_name: &str,
        difference: &JobDifference,
    ) -> Result<()> {
        let jobset_info = self.db_service.get_jobset_info(jobset_id).await?;
        let octocrab = self.octocrab_for_owner(&jobset_info.owner)?;

        let drv = self.db_service.get_drv(drv_id).await?;
        let state = drv
            .map(|x| x.build_state)
            .unwrap_or(DrvBuildState::Completed(
                crate::db::model::build_event::DrvBuildResult::Failure,
            ));

        let ci_check_info = CICheckInfo {
            commit: jobset_info.sha.clone(),
            base_commit: None,
            owner: jobset_info.owner.clone(),
            repo_name: jobset_info.repo_name.clone(),
        };

        let check_run = ci_check_info
            .create_gh_check_run(
                &octocrab,
                &jobset_info.job,
                job_attr_name,
                state,
                difference,
            )
            .await?;

        self.db_service
            .insert_check_run_info(
                check_run.id.0 as i64,
                drv_id,
                &jobset_info.repo_name,
                &jobset_info.owner,
            )
            .await?;
        Ok(())
    }

    pub(super) async fn handle_create_check_run(
        &self,
        owner: &str,
        repo_name: &str,
        sha: &str,
        check_name: &str,
        check_result_id: i64,
    ) -> Result<()> {
        let octocrab = self.octocrab_for_owner(owner)?;
        let check_run =
            actions::create_check_run(&octocrab, owner, repo_name, sha, check_name).await?;
        self.db_service
            .insert_check_run_info_for_check(
                check_run.id.0 as i64,
                check_result_id,
                repo_name,
                owner,
            )
            .await?;
        Ok(())
    }
}
