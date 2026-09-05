// GitHub task dispatcher

use anyhow::{Context, Result};
use tracing::{debug, warn};

use super::GitHubService;
use crate::github::service::{GitHubTask, actions};

impl GitHubService {
    pub(super) async fn handle_github_task(&self, task: &GitHubTask) -> Result<()> {
        use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

        match task {
            GitHubTask::UpdateBuildStatus { drv_id, status } => {
                let check_runs = self.db_service.check_runs_for_drv_path(drv_id).await?;

                // For failures, fetch the last 25 lines of the build log
                let log_tail = if status.is_failure() {
                    self.fetch_log_tail(drv_id, 25).await
                } else {
                    None
                };

                let (gql_status, gql_conclusion) =
                    crate::github::service::graphql_batch::build_state_to_graphql(status);

                for check_run in check_runs {
                    // Use GraphQL batcher if node_id is available, otherwise fall back to REST
                    if let Some(node_id) = &check_run.node_id {
                        let repo_node_id = self
                            .get_repo_node_id(&check_run.repo_owner, &check_run.repo_name)
                            .await;
                        if let Some(repo_node_id) = repo_node_id {
                            self.batcher
                                .queue_update_with_log(
                                    &check_run.repo_owner,
                                    &repo_node_id,
                                    node_id,
                                    gql_status,
                                    gql_conclusion,
                                    log_tail.clone(),
                                )
                                .await;
                            continue;
                        }
                    }
                    // Fallback: REST API for check runs without node_id
                    debug!("Updating checkrun status of {} (REST fallback)", &check_run.check_run_id);
                    self.rate_limiter.acquire().await;
                    let octocrab = self.octocrab_for_owner(&check_run.repo_owner)?;
                    check_run
                        .send_gh_update_with_log(&octocrab, status, log_tail.as_deref())
                        .await?;
                }
            },
            GitHubTask::UpdateBuildStatusWithSizeWarning {
                drv_id,
                status,
                baseline_size,
                current_size,
                increase_percent,
                threshold_percent,
            } => {
                self.handle_update_build_status_with_size_warning(
                    drv_id,
                    status,
                    *baseline_size,
                    *current_size,
                    *increase_percent,
                    *threshold_percent,
                )
                .await?;
            },
            GitHubTask::CreateJobSet {
                ci_check_info,
                name,
                jobs,
                config_json,
            } => {
                self.create_job_set(ci_check_info, name, jobs, config_json.as_deref())
                    .await?;
            },
            GitHubTask::CreateCIConfigureGate { ci_check_info } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                let check_run = actions::create_ci_configure_gate(&octocrab, ci_check_info).await?;
                self.github_configure_checks
                    .lock()
                    .await
                    .insert(ci_check_info.commit.clone(), check_run.id);
            },
            GitHubTask::CompleteCIConfigureGate { ci_check_info } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                let check_run_id = self
                    .github_configure_checks
                    .lock()
                    .await
                    .remove(&ci_check_info.commit)
                    .context("No configure gate check run found for commit")?;
                actions::update_ci_configure_gate(
                    &octocrab,
                    ci_check_info,
                    check_run_id,
                    CheckRunStatus::Completed,
                    CheckRunConclusion::Success,
                )
                .await?;
            },
            GitHubTask::CreateCIEvalJob {
                ci_check_info,
                job_title,
            } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                let check_run =
                    actions::create_ci_eval_job(&octocrab, job_title, ci_check_info).await?;
                self.github_eval_checks.lock().await.insert(
                    (ci_check_info.commit.clone(), job_title.clone()),
                    check_run.id,
                );
            },
            GitHubTask::CompleteCIEvalJob {
                ci_check_info,
                job_name,
                conclusion,
            } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                // Try in-memory map first, then fall back to GitHub API lookup
                // (the map is empty after server restarts)
                let check_run_id = match self
                    .github_eval_checks
                    .lock()
                    .await
                    .remove(&(ci_check_info.commit.clone(), job_name.clone()))
                {
                    Some(id) => id,
                    None => {
                        // Look up the check run by name from GitHub API
                        let check_name = format!("EkaCI: Evaluate Job ({})", job_name);
                        match actions::find_check_run_by_name(
                            &octocrab,
                            &ci_check_info.owner,
                            &ci_check_info.repo_name,
                            &ci_check_info.commit,
                            &check_name,
                        )
                        .await
                        {
                            Ok(Some(id)) => id,
                            Ok(None) => {
                                warn!(
                                    "No eval gate check run found for {}/{} commit {} job {}",
                                    ci_check_info.owner,
                                    ci_check_info.repo_name,
                                    ci_check_info.commit,
                                    job_name
                                );
                                return Ok(());
                            },
                            Err(e) => {
                                warn!(
                                    "Failed to look up eval gate check run: {:?}",
                                    e
                                );
                                return Ok(());
                            },
                        }
                    },
                };
                actions::update_ci_eval_job(
                    &octocrab,
                    ci_check_info,
                    check_run_id,
                    CheckRunStatus::Completed,
                    (*conclusion).into(),
                )
                .await?;
            },
            GitHubTask::CancelCheckRunsForCommit { ci_check_info } => {
                self.handle_cancel_check_runs_for_commit(ci_check_info)
                    .await?;
            },
            GitHubTask::CreateFailureCheckRun {
                drv_id,
                jobset_id,
                job_attr_name,
                difference,
            } => {
                self.handle_create_failure_check_run(drv_id, *jobset_id, job_attr_name, difference)
                    .await?;
            },
            GitHubTask::CreateApprovalRequiredCheckRun {
                ci_check_info,
                username,
            } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                actions::create_approval_required_check_run(&octocrab, ci_check_info, username)
                    .await?;
            },
            GitHubTask::FailCIEvalJob {
                ci_check_info,
                job_name,
                errors,
            } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                actions::fail_ci_eval_job(&octocrab, ci_check_info, job_name, errors).await?;
            },
            GitHubTask::CreateCheckRun {
                owner,
                repo_name,
                sha,
                check_name,
                check_result_id,
            } => {
                self.handle_create_check_run(owner, repo_name, sha, check_name, *check_result_id)
                    .await?;
            },
            GitHubTask::CheckComplete(result) => {
                let octocrab = self.octocrab_for_owner(&result.owner)?;
                actions::update_check_run(
                    &octocrab,
                    &result.owner,
                    &result.repo_name,
                    result.check_run_id,
                    &result.check_name,
                    result.success,
                    result.exit_code,
                    &result.stdout,
                    &result.stderr,
                    result.duration_ms,
                )
                .await?;
            },
            GitHubTask::CheckFailed(result) => {
                let octocrab = self.octocrab_for_owner(&result.owner)?;
                actions::update_check_run(
                    &octocrab,
                    &result.owner,
                    &result.repo_name,
                    result.check_run_id,
                    &result.check_name,
                    result.success,
                    result.exit_code,
                    &result.stdout,
                    &result.stderr,
                    result.duration_ms,
                )
                .await?;
            },
            GitHubTask::CheckAutoMerge {
                owner,
                repo_name,
                pr_number,
            } => {
                self.handle_check_auto_merge(owner, repo_name, *pr_number)
                    .await?;
            },
            GitHubTask::CreateDependencyChangesGate {
                ci_check_info,
                jobset_id,
                base_jobset_id,
            } => {
                self.handle_create_dependency_changes_gate(
                    ci_check_info,
                    *jobset_id,
                    *base_jobset_id,
                )
                .await?;
            },
            GitHubTask::CreateChangeSummaryCheck { ci_check_info, job } => {
                self.handle_create_change_summary_check(ci_check_info, job)
                    .await?;
            },
            GitHubTask::CreateChannelPromotionCheck {
                owner,
                repo_name,
                sha,
                channel_name,
                promotion_status,
                blocked_reason,
            } => {
                self.handle_create_channel_promotion_check(
                    owner,
                    repo_name,
                    sha,
                    channel_name,
                    *promotion_status,
                    blocked_reason.as_deref(),
                )
                .await?;
            },
            GitHubTask::ProcessMergeCommand {
                owner,
                repo_name,
                pr_number,
                comment_id,
                requester_id,
                requester_login,
                body,
                comment_created_at,
            } => {
                self.handle_process_merge_command(
                    owner,
                    repo_name,
                    *pr_number,
                    *comment_id,
                    *requester_id,
                    requester_login,
                    body,
                    comment_created_at,
                )
                .await?;
            },
            GitHubTask::CommentMergeDriftCancelled {
                owner,
                repo_name,
                pr_number,
                expected_sha,
                actual_sha,
                requester_login,
            } => {
                self.handle_comment_merge_drift_cancelled(
                    owner,
                    repo_name,
                    *pr_number,
                    expected_sha,
                    actual_sha,
                    requester_login,
                )
                .await?;
            },
            GitHubTask::ReactToComment {
                owner,
                repo_name,
                comment_id,
                content,
            } => {
                let octocrab = self.octocrab_for_owner(owner)?;
                if let Err(e) =
                    actions::add_comment_reaction(&octocrab, owner, repo_name, *comment_id, content)
                        .await
                {
                    warn!(
                        "Failed to react {} to comment {} on {}/{}: {:?}",
                        content, comment_id, owner, repo_name, e
                    );
                }
            },
            GitHubTask::PostIssueComment {
                owner,
                repo_name,
                issue_number,
                body,
            } => {
                let octocrab = self.octocrab_for_owner(owner)?;
                if let Err(e) =
                    actions::post_issue_comment(&octocrab, owner, repo_name, *issue_number, body)
                        .await
                {
                    warn!(
                        "Failed to post comment on {}/{}#{}: {:?}",
                        owner, repo_name, issue_number, e
                    );
                }
            },
            GitHubTask::ResyncCheckRuns { sha } => {
                self.handle_resync_check_runs(sha).await?;
            },
        }
        Ok(())
    }
}
