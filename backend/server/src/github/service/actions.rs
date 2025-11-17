use anyhow::Result;
use octocrab::Octocrab;
use octocrab::models::CheckRunId;
use octocrab::models::checks::CheckRun;
use octocrab::params::checks::CheckRunConclusion;
use tracing::debug;

use crate::github::CICheckInfo;

/// This will send an initial ci gate which is used to determine what gates
/// are relevant for a PR
pub async fn create_ci_configure_gate(
    octocrab: &Octocrab,
    ci_check_info: &CICheckInfo,
) -> Result<CheckRun> {
    use octocrab::params::checks::CheckRunStatus;

    debug!(
        "Creating CI configure gate check run for commit {}",
        &ci_check_info.commit
    );

    // Create a check run for the CI configuration gate
    let check_run = octocrab
        //.installation(InstallationId(95084816))?
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .create_check_run("EkaCI: Configure", &ci_check_info.commit)
        .status(CheckRunStatus::InProgress)
        .send()
        .await?;

    debug!(
        "Successfully created CI configure gate check run for commit #{}",
        &ci_check_info.commit
    );

    Ok(check_run)
}

pub async fn update_ci_configure_gate(
    octocrab: &Octocrab,
    ci_check_info: &CICheckInfo,
    check_run_id: CheckRunId,
    status: octocrab::params::checks::CheckRunStatus,
    conclusion: CheckRunConclusion,
) -> Result<()> {
    debug!(
        "Updating CI configure gate check run {} with status {:?}",
        check_run_id, status
    );

    octocrab
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .update_check_run(check_run_id)
        .status(status)
        .conclusion(conclusion)
        .send()
        .await?;

    debug!(
        "Successfully updated CI configure gate check run {}",
        check_run_id
    );

    Ok(())
}
