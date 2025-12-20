use anyhow::Result;
use octocrab::Octocrab;
use octocrab::models::CheckRunId;
use octocrab::models::checks::CheckRun;
use octocrab::params::checks::CheckRunConclusion;
use tracing::debug;

use crate::github::CICheckInfo;
use crate::nix::nix_eval_jobs::NixEvalError;

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

/// This will send an initial ci eval job gate which is used to determine what jobs
/// are being evaluated for a PR
pub async fn create_ci_eval_job(
    octocrab: &Octocrab,
    job_title: &str,
    ci_check_info: &CICheckInfo,
) -> Result<CheckRun> {
    use octocrab::params::checks::CheckRunStatus;

    debug!(
        "Creating CI eval job check run for commit {}",
        &ci_check_info.commit
    );

    let title = format!("EkaCI: Evaluate Job ({})", job_title);
    // Create a check run for the CI eval job gate
    let check_run = octocrab
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .create_check_run(&title, &ci_check_info.commit)
        .status(CheckRunStatus::InProgress)
        .send()
        .await?;

    debug!(
        "Successfully created CI eval job check run for commit #{}",
        &ci_check_info.commit
    );

    Ok(check_run)
}

pub async fn update_ci_eval_job(
    octocrab: &Octocrab,
    ci_check_info: &CICheckInfo,
    check_run_id: CheckRunId,
    status: octocrab::params::checks::CheckRunStatus,
    conclusion: CheckRunConclusion,
) -> Result<()> {
    debug!(
        "Updating CI eval job check run {} with status {:?}",
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
        "Successfully updated CI eval job check run {}",
        check_run_id
    );

    Ok(())
}

/// Create a neutral check run indicating that approval is required before builds can run
pub async fn create_approval_required_check_run(
    octocrab: &Octocrab,
    ci_check_info: &CICheckInfo,
    username: &str,
) -> Result<CheckRun> {
    use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

    debug!(
        "Creating approval required check run for commit {} (user: {})",
        &ci_check_info.commit, username
    );

    let title = format!("EkaCI: Approval Required (User: @{})", username);

    let check_run = octocrab
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .create_check_run(&title, &ci_check_info.commit)
        .status(CheckRunStatus::Completed)
        .conclusion(CheckRunConclusion::Neutral)
        .send()
        .await?;

    debug!(
        "Successfully created approval required check run for commit #{}",
        &ci_check_info.commit
    );

    Ok(check_run)
}

/// Fail a CI eval job gate due to evaluation errors
pub async fn fail_ci_eval_job(
    octocrab: &Octocrab,
    ci_check_info: &CICheckInfo,
    job_name: &str,
    errors: &[NixEvalError],
) -> Result<CheckRun> {
    use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

    debug!(
        "Creating failed CI eval job check run for job {} on commit {} with {} errors",
        job_name,
        &ci_check_info.commit,
        errors.len()
    );

    let title = format!("EkaCI: Evaluate Job ({})", job_name);

    // Format error details for the check run output
    let mut summary = format!(
        "# Evaluation Failed\n\n{} evaluation error(s) occurred:\n\n",
        errors.len()
    );

    for (idx, error) in errors.iter().enumerate() {
        summary.push_str(&format!("## Error {}\n\n", idx + 1));
        summary.push_str(&format!("**Attribute:** `{}`\n\n", error.attr));
        summary.push_str(&format!("**Error:**\n```\n{}\n```\n\n", error.error));
    }

    let check_run_output = octocrab::params::checks::CheckRunOutput {
        title: "Evaluation errors".to_string(),
        summary,
        text: None,
        annotations: vec![],
        images: vec![],
    };
    let check_run = octocrab
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .create_check_run(&title, &ci_check_info.commit)
        .status(CheckRunStatus::Completed)
        .conclusion(CheckRunConclusion::Failure)
        .output(check_run_output)
        .send()
        .await?;

    debug!(
        "Successfully created failed CI eval job check run for commit #{}",
        &ci_check_info.commit
    );

    Ok(check_run)
}
