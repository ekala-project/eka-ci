use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use octocrab::Octocrab;
use octocrab::models::{CheckRunId, Installation};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::db::DbService;
use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;
use crate::dependency_comparison;
use crate::graph::GraphServiceHandle;
use crate::metrics::ChangeSummaryMetrics;
use crate::nix::nix_eval_jobs::NixEvalDrv;

/// Debounce window before the aggregated change-summary check is posted.
pub(crate) const CHANGE_SUMMARY_DEBOUNCE: Duration = Duration::from_secs(5 * 60);

pub mod actions;
mod types;

pub use types::{CICheckInfo, Commit, GitHubTask, JobDifference, Owner};

/// This service will be response for pushing/posting events to GitHub
pub struct GitHubService {
    // Channels to individual services
    // We may in the future need to recover an individual service, so retaining
    // a handle to the other service channels will be prequisite
    db_service: DbService,
    octocrab: Octocrab,
    installations: HashMap<Owner, Installation>,
    github_receiver: mpsc::Receiver<GitHubTask>,
    github_sender: mpsc::Sender<GitHubTask>,
    github_configure_checks: HashMap<Commit, CheckRunId>,
    github_eval_checks: HashMap<(Commit, String), CheckRunId>,
    /// Tracks the change-summary check id per head SHA for idempotent updates.
    change_summary_checks: HashMap<Commit, CheckRunId>,
    /// Dedup guard so a single debounce timer fires per head SHA.
    change_summary_pending: HashSet<Commit>,
    /// Graph handle used to compute rebuild-impact for change summaries.
    graph_handle: GraphServiceHandle,
    /// Optional metrics for change-summary pipeline observability.
    change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
}

impl GitHubService {
    pub async fn new(
        db_service: DbService,
        octocrab: Octocrab,
        graph_handle: GraphServiceHandle,
        change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
    ) -> anyhow::Result<Self> {
        use futures::stream::TryStreamExt;
        use tokio::pin;

        let mut installations = HashMap::new();
        let octoclone = octocrab.clone();
        let installations_stream = octocrab
            .apps()
            .installations()
            .send()
            .await?
            .into_stream(&octoclone);
        pin!(installations_stream);
        while let Some(installation) = installations_stream.try_next().await? {
            installations.insert(installation.account.login.clone(), installation);
        }

        debug!("Installations: {:?}", &installations);

        // Sync installations and repositories to database
        info!("Syncing {} installations to database", installations.len());
        for installation in installations.values() {
            // Persist installation to database
            if let Err(e) = db_service
                .upsert_installation(
                    installation.id.0 as i64,
                    &installation.account.r#type,
                    &installation.account.login,
                )
                .await
            {
                warn!(
                    "Failed to persist installation {} ({}): {:?}",
                    installation.id.0, installation.account.login, e
                );
                continue;
            }

            debug!(
                "Persisted installation {} ({})",
                installation.id.0, installation.account.login
            );

            // Fetch and persist repositories for this installation
            // Use installation-scoped octocrab to access /installation/repositories endpoint
            let installation_octo = match octoclone.installation(installation.id) {
                Ok(inst_octo) => inst_octo,
                Err(e) => {
                    warn!(
                        "Failed to create installation client for {} ({}): {:?}",
                        installation.id.0, installation.account.login, e
                    );
                    continue;
                },
            };

            // Call GET /installation/repositories
            let repos_response: Result<octocrab::Page<octocrab::models::Repository>, _> =
                installation_octo
                    .get("/installation/repositories", None::<&()>)
                    .await;

            let repos_page = match repos_response {
                Ok(page) => page,
                Err(e) => {
                    warn!(
                        "Failed to fetch repositories for installation {} ({}): {:?}",
                        installation.id.0, installation.account.login, e
                    );
                    continue;
                },
            };

            let repos_stream = repos_page.into_stream(&installation_octo);
            pin!(repos_stream);
            let mut repo_count = 0;

            while let Some(repo_result) = repos_stream.try_next().await.transpose() {
                match repo_result {
                    Ok(repo) => {
                        let repo_owner =
                            repo.owner.as_ref().map(|o| o.login.as_str()).unwrap_or("");

                        if let Err(e) = db_service
                            .upsert_installation_repository(
                                installation.id.0 as i64,
                                repo.id.0 as i64,
                                &repo.name,
                                repo_owner,
                            )
                            .await
                        {
                            warn!(
                                "Failed to persist repository {}/{} for installation {}: {:?}",
                                repo_owner, repo.name, installation.id.0, e
                            );
                        } else {
                            repo_count += 1;
                        }
                    },
                    Err(e) => {
                        warn!(
                            "Error fetching repository for installation {} ({}): {:?}",
                            installation.id.0, installation.account.login, e
                        );
                    },
                }
            }

            info!(
                "Synced {} repositories for installation {} ({})",
                repo_count, installation.id.0, installation.account.login
            );
        }

        let (github_sender, github_receiver) = mpsc::channel(100);
        Ok(Self {
            db_service,
            octocrab,
            installations,
            github_receiver,
            github_sender,
            github_configure_checks: HashMap::new(),
            github_eval_checks: HashMap::new(),
            change_summary_checks: HashMap::new(),
            change_summary_pending: HashSet::new(),
            graph_handle,
            change_summary_metrics,
        })
    }

    pub fn get_sender(&self) -> mpsc::Sender<GitHubTask> {
        self.github_sender.clone()
    }

    pub fn run(self, cancel_token: CancellationToken) -> JoinHandle<()> {
        tokio::spawn(async move {
            cancel_token
                .run_until_cancelled(self.github_tasks_loop())
                .await;
        })
    }

    /// Attempt to look up installation by owner
    fn octocrab_for_owner(&self, owner: &str) -> Result<Octocrab> {
        let installation = self
            .installations
            .get(owner)
            .context("No installation associated with owner")?;
        debug!(
            "Found installation for owner {}: {:?}",
            &owner, &installation
        );
        let octo = self.octocrab.installation(installation.id)?;
        Ok(octo)
    }

    async fn github_tasks_loop(mut self) {
        loop {
            if let Some(task) = self.github_receiver.recv().await {
                if let Err(e) = self.handle_github_task(&task).await {
                    warn!("Failed to handle github request {:?}: {:?}", &task, e);
                }
            }
        }
    }

    async fn create_job_set(
        &mut self,
        ci_check_info: &std::sync::Arc<CICheckInfo>,
        name: &str,
        jobs: &[NixEvalDrv],
        config_json: Option<&str>,
    ) -> Result<()> {
        let jobset_id = self
            .db_service
            .create_github_jobset_with_jobs(
                &ci_check_info.commit,
                name,
                &ci_check_info.owner,
                &ci_check_info.repo_name,
                jobs,
                config_json,
            )
            .await?;

        // This is only relevant on PRs, missing a base commit denotes that
        // this jobset creation is done for a base_commit
        if let Some(base_commit) = ci_check_info.base_commit.as_ref() {
            let (new_jobs, changed_drvs, _removed_jobs) = self
                .db_service
                .job_difference(&ci_check_info.commit, base_commit, name)
                .await?;

            // Update the Job table to mark which jobs are new/changed
            let new_drv_ids: Vec<DrvId> = new_jobs.iter().map(|d| d.drv_path.clone()).collect();
            let changed_drv_ids: Vec<DrvId> =
                changed_drvs.iter().map(|d| d.drv_path.clone()).collect();

            self.db_service
                .update_job_differences(jobset_id, &new_drv_ids, &changed_drv_ids)
                .await?;

            // Note: We no longer create check_runs eagerly here
            // Check_runs will be created lazily when jobs fail

            // Queue dependency changes gate creation
            // This needs the base jobset ID to compare dependencies
            let base_jobset_id: Option<i64> =
                sqlx::query_scalar("SELECT ROWID FROM GitHubJobSets WHERE sha = ? AND job = ?")
                    .bind(base_commit)
                    .bind(name)
                    .fetch_optional(&self.db_service.pool)
                    .await?;

            if let Some(base_jobset_id) = base_jobset_id {
                self.github_sender
                    .send(GitHubTask::CreateDependencyChangesGate {
                        ci_check_info: std::sync::Arc::clone(ci_check_info),
                        jobset_id,
                        base_jobset_id,
                    })
                    .await?;
            }

            // Schedule the aggregated change-summary check, deduped per head SHA.
            if self
                .change_summary_pending
                .insert(ci_check_info.commit.clone())
            {
                self.spawn_change_summary_debounce(Arc::clone(ci_check_info), name.to_string());
            }
        }
        Ok(())
    }

    /// Spawn the debounce timer that enqueues a `CreateChangeSummaryCheck`.
    fn spawn_change_summary_debounce(&self, ci_check_info: Arc<CICheckInfo>, job: String) {
        let sender = self.github_sender.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CHANGE_SUMMARY_DEBOUNCE).await;
            if let Err(e) = sender
                .send(GitHubTask::CreateChangeSummaryCheck { ci_check_info, job })
                .await
            {
                warn!(
                    "Failed to enqueue CreateChangeSummaryCheck after debounce: {:?}",
                    e
                );
            }
        });
    }

    async fn handle_github_task(&mut self, task: &GitHubTask) -> Result<()> {
        use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

        match task {
            GitHubTask::UpdateBuildStatus { drv_id, status } => {
                let check_runs = self.db_service.check_runs_for_drv_path(drv_id).await?;

                for check_run in check_runs {
                    debug!("Updating checkrun status of {}", &check_run.check_run_id);
                    let octocrab = self.octocrab_for_owner(&check_run.repo_owner)?;
                    check_run.send_gh_update(&octocrab, status).await?;
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
                    .insert(ci_check_info.commit.clone(), check_run.id);
            },
            GitHubTask::CompleteCIConfigureGate { ci_check_info } => {
                let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;
                let check_run_id = self
                    .github_configure_checks
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
                self.github_eval_checks.insert(
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
                let check_run_id = self
                    .github_eval_checks
                    .remove(&(ci_check_info.commit.clone(), job_name.clone()))
                    .context("No eval job check run found for commit")?;
                actions::update_ci_eval_job(
                    &octocrab,
                    ci_check_info,
                    check_run_id,
                    CheckRunStatus::Completed,
                    *conclusion,
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
        }
        Ok(())
    }

    async fn handle_update_build_status_with_size_warning(
        &self,
        drv_id: &DrvId,
        status: &DrvBuildState,
        baseline_size: u64,
        current_size: u64,
        increase_percent: f64,
        threshold_percent: f64,
    ) -> Result<()> {
        let check_runs = self.db_service.check_runs_for_drv_path(drv_id).await?;
        for check_run in check_runs {
            debug!(
                "Updating checkrun with size warning for {}",
                &check_run.check_run_id
            );
            let octocrab = self.octocrab_for_owner(&check_run.repo_owner)?;
            actions::update_check_run_with_size_warning(
                &octocrab,
                &check_run,
                status,
                baseline_size,
                current_size,
                increase_percent,
                threshold_percent,
            )
            .await?;
        }
        Ok(())
    }

    async fn handle_cancel_check_runs_for_commit(
        &mut self,
        ci_check_info: &CICheckInfo,
    ) -> Result<()> {
        use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

        let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

        // Cancel any in-progress configure gate.
        if let Some(check_run_id) = self.github_configure_checks.remove(&ci_check_info.commit) {
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
            .keys()
            .filter(|(commit, _)| commit == &ci_check_info.commit)
            .cloned()
            .collect();

        for key in keys_to_remove {
            if let Some(check_run_id) = self.github_eval_checks.remove(&key) {
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

    async fn handle_create_failure_check_run(
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

    async fn handle_create_check_run(
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

    async fn handle_create_dependency_changes_gate(
        &self,
        ci_check_info: &CICheckInfo,
        jobset_id: i64,
        base_jobset_id: i64,
    ) -> Result<()> {
        let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

        debug!(
            "Creating dependency changes gate for commit {} (jobset: {}, base: {})",
            &ci_check_info.commit, jobset_id, base_jobset_id
        );

        let comparisons = dependency_comparison::compare_runtime_references_for_jobset(
            base_jobset_id,
            jobset_id,
            &self.db_service.pool,
        )
        .await?;

        let dependency_diff =
            dependency_comparison::format_dependency_changes_as_diff(&comparisons);

        actions::create_dependency_changes_gate(
            &octocrab,
            ci_check_info,
            &dependency_diff,
            comparisons.len(),
        )
        .await?;

        debug!(
            "Successfully created dependency changes gate with {} packages affected",
            comparisons.len()
        );
        Ok(())
    }

    /// Idempotently post (or patch) the aggregated change-summary check for a head SHA.
    async fn handle_create_change_summary_check(
        &mut self,
        ci_check_info: &Arc<CICheckInfo>,
        job: &str,
    ) -> Result<()> {
        self.change_summary_pending.remove(&ci_check_info.commit);

        let Some(base_sha) = ci_check_info.base_commit.as_deref() else {
            debug!(
                "Skipping change-summary check for {}: no base commit (not a PR head)",
                &ci_check_info.commit
            );
            return Ok(());
        };

        let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

        let summary = match crate::change_summary::build_change_summary(
            &self.db_service.pool,
            &self.graph_handle,
            &ci_check_info.commit,
            base_sha,
            job,
            crate::change_summary::DEFAULT_MAX_PACKAGES_LISTED,
            crate::change_summary::impact::DEFAULT_MAX_TOP_BLAST_RADIUS,
            self.change_summary_metrics.as_deref(),
        )
        .await
        {
            Ok(Some(s)) => s,
            Ok(None) => {
                debug!(
                    "No change-summary built for {} (head/base jobset missing); skipping check \
                     post",
                    &ci_check_info.commit
                );
                return Ok(());
            },
            Err(e) => {
                warn!(
                    "Failed to build change-summary for commit {}: {:?}",
                    &ci_check_info.commit, e
                );
                return Ok(());
            },
        };

        let markdown = summary.markdown;

        if let Some(existing) = self
            .change_summary_checks
            .get(&ci_check_info.commit)
            .copied()
        {
            if let Err(e) =
                actions::update_change_summary_check(&octocrab, ci_check_info, existing, markdown)
                    .await
            {
                warn!(
                    "Failed to update change-summary check {} for {}: {:?}",
                    existing, &ci_check_info.commit, e
                );
            }
        } else {
            match actions::create_change_summary_check(&octocrab, ci_check_info, markdown).await {
                Ok(check_run) => {
                    self.change_summary_checks
                        .insert(ci_check_info.commit.clone(), check_run.id);
                },
                Err(e) => {
                    warn!(
                        "Failed to create change-summary check for {}: {:?}",
                        &ci_check_info.commit, e
                    );
                },
            }
        }

        Ok(())
    }

    async fn handle_comment_merge_drift_cancelled(
        &self,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        expected_sha: &str,
        actual_sha: &str,
        requester_login: &str,
    ) -> Result<()> {
        let octocrab = self.octocrab_for_owner(owner)?;
        let body = format!(
            "@{} your `@eka-ci merge` request was cancelled because new commits landed on this PR \
             since you issued the command.\n\n- expected head: `{}`\n- current head: `{}`\n\nIf \
             you still want to merge, re-issue `@eka-ci merge` on the updated PR.",
            requester_login,
            short_sha(expected_sha),
            short_sha(actual_sha),
        );
        if let Err(e) =
            actions::post_issue_comment(&octocrab, owner, repo_name, pr_number, &body).await
        {
            warn!(
                "Failed to post SHA-drift comment on {}/{}#{}: {:?}",
                owner, repo_name, pr_number, e
            );
        }
        Ok(())
    }

    // ---- Auto-merge evaluator ----

    async fn handle_check_auto_merge(
        &self,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
    ) -> Result<()> {
        let octocrab = self.octocrab_for_owner(owner)?;

        info!(
            "Checking auto-merge eligibility for PR #{} in {}/{}",
            pr_number, owner, repo_name
        );

        // Defer until head-commit jobset has fully succeeded. Idempotent
        // with the scheduler-driven trigger; defends the review-driven
        // path from merging in-flight or already-failed builds.
        if !crate::db::github::pr_head_build_succeeded(
            pr_number,
            owner,
            repo_name,
            &self.db_service.pool,
        )
        .await?
        {
            info!(
                "PR #{} head build not yet successful, deferring auto-merge",
                pr_number
            );
            return Ok(());
        }

        // Look up by (owner, repo, pr_number) — not head_sha, which
        // may have drifted. Need the raw row for `comment_merge_*`.
        let Some(pr) = crate::db::github::get_pull_request_row(
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?
        else {
            warn!(
                "PR #{} not found in {}/{} while evaluating auto-merge",
                pr_number, owner, repo_name
            );
            return Ok(());
        };

        let pending_cmt_merge = pr.pending_comment_merge();

        // SHA-drift check: comment-merges are pinned to a commit.
        if let Some(cmr) = pending_cmt_merge.as_ref() {
            if cmr.sha != pr.head_sha {
                self.cancel_drifted_comment_merge(owner, repo_name, pr_number, cmr, &pr.head_sha)
                    .await?;
                return Ok(());
            }
        }

        // At least one merge path must be active.
        if !pr.auto_merge_enabled && pending_cmt_merge.is_none() {
            debug!(
                "PR #{} in {}/{} has no active auto-merge or comment-merge request; skipping",
                pr_number, owner, repo_name
            );
            return Ok(());
        }

        let changed_packages = crate::db::github::get_pr_changed_packages(
            pr_number,
            owner,
            repo_name,
            &self.db_service.pool,
        )
        .await?;

        if changed_packages.is_empty() {
            info!(
                "PR #{} has no changed packages, skipping auto-merge",
                pr_number
            );
            return Ok(());
        }

        // Maintainer-approval gate. Skipped for comment-driven merges —
        // authority was verified at ProcessMergeCommand time. UI
        // auto-merge still requires per-package approvals.
        if pending_cmt_merge.is_none() {
            let (eligible, missing_approvals) = actions::check_pr_maintainer_approvals(
                &octocrab,
                owner,
                repo_name,
                pr_number as u64,
                &changed_packages,
                &self.db_service.pool,
            )
            .await?;

            if !eligible {
                info!(
                    "PR #{} is not eligible for auto-merge. Missing approvals for packages: {:?}",
                    pr_number, missing_approvals
                );
                return Ok(());
            }
        }

        // Method: comment request → PR-stored preference → squash.
        let merge_method = pending_cmt_merge
            .as_ref()
            .and_then(|cmr| cmr.method.as_deref())
            .or(pr.merge_method.as_deref())
            .unwrap_or("squash");

        // Validate against repo settings before trying.
        match actions::validate_merge_method(&octocrab, owner, repo_name, merge_method).await {
            Ok(actions::MergeMethodCheck::Ok) => {},
            Ok(actions::MergeMethodCheck::NotAllowed { allowed }) => {
                warn!(
                    "PR #{} in {}/{}: configured merge method '{}' is not allowed by repository \
                     settings (allowed: {:?}); skipping auto-merge",
                    pr_number, owner, repo_name, merge_method, allowed
                );
                return Ok(());
            },
            Err(e) => {
                warn!(
                    "PR #{} in {}/{}: failed to fetch repository merge settings: {:?}; skipping \
                     auto-merge",
                    pr_number, owner, repo_name, e
                );
                return Ok(());
            },
        }

        self.auto_merge_execute(
            &octocrab,
            owner,
            repo_name,
            pr_number,
            merge_method,
            pending_cmt_merge.as_ref(),
        )
        .await;

        Ok(())
    }

    /// Notify requester, react `:confused:`, and clear the pending row
    /// when a comment-merge's pinned SHA no longer matches the PR head.
    async fn cancel_drifted_comment_merge(
        &self,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        cmr: &crate::db::github::CommentMergeRequest,
        current_head: &str,
    ) -> Result<()> {
        warn!(
            "PR #{} in {}/{}: comment-merge SHA drift (requested {}, now {}); cancelling",
            pr_number, owner, repo_name, cmr.sha, current_head
        );

        // Best-effort notifications; errors logged downstream.
        let _ = self
            .github_sender
            .send(GitHubTask::CommentMergeDriftCancelled {
                owner: owner.to_string(),
                repo_name: repo_name.to_string(),
                pr_number,
                expected_sha: cmr.sha.clone(),
                actual_sha: current_head.to_string(),
                requester_login: cmr.requester_login.clone(),
            })
            .await;
        let _ = self
            .github_sender
            .send(GitHubTask::ReactToComment {
                owner: owner.to_string(),
                repo_name: repo_name.to_string(),
                comment_id: cmr.comment_id,
                content: "confused",
            })
            .await;

        crate::db::github::clear_comment_merge_request(
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?;
        Ok(())
    }

    /// Execute the merge + record post-conditions. Infallible at the
    /// caller level — a failed merge is logged and swallowed (the next
    /// trigger will retry).
    async fn auto_merge_execute(
        &self,
        octocrab: &Octocrab,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        merge_method: &str,
        pending_cmt_merge: Option<&crate::db::github::CommentMergeRequest>,
    ) {
        info!(
            "Auto-merging PR #{} in {}/{} using method '{}'",
            pr_number, owner, repo_name, merge_method
        );

        match actions::merge_pull_request(
            octocrab,
            owner,
            repo_name,
            pr_number as u64,
            merge_method,
            None,
            None,
        )
        .await
        {
            Ok(_) => {
                info!(
                    "Successfully auto-merged PR #{} in {}/{}",
                    pr_number, owner, repo_name
                );

                // Capture comment_id before `mark_pr_merged` clears the
                // pending row; used below for the rocket ack.
                let pending_comment_id = pending_cmt_merge.map(|c| c.comment_id);

                if let Err(e) = crate::db::github::mark_pr_merged(
                    owner,
                    repo_name,
                    pr_number,
                    pending_cmt_merge.map(|c| c.requester_id),
                    &self.db_service.pool,
                )
                .await
                {
                    warn!(
                        "Failed to mark PR #{} as merged in database: {:?}",
                        pr_number, e
                    );
                }

                if let Some(comment_id) = pending_comment_id {
                    let _ = self
                        .github_sender
                        .send(GitHubTask::ReactToComment {
                            owner: owner.to_string(),
                            repo_name: repo_name.to_string(),
                            comment_id,
                            content: "rocket",
                        })
                        .await;
                }
            },
            Err(e) => {
                warn!(
                    "Failed to auto-merge PR #{} in {}/{}: {:?}",
                    pr_number, owner, repo_name, e
                );
            },
        }
    }

    // ---- Comment-command handler ----

    #[allow(clippy::too_many_arguments)]
    async fn handle_process_merge_command(
        &self,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        comment_id: i64,
        requester_id: i64,
        requester_login: &str,
        body: &str,
        comment_created_at: &chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        use crate::github::webhook::comment_command::{CommentCommand, parse_comment_command};

        let octocrab = self.octocrab_for_owner(owner)?;

        // Re-parse rather than carrying a typed command — payload stays
        // POD and parsing stays in one place.
        let Some(cmd) = parse_comment_command(body) else {
            debug!(
                "Comment {} on {}/{}#{} no longer parses as a command; dropping",
                comment_id, owner, repo_name, pr_number
            );
            return Ok(());
        };

        match cmd {
            CommentCommand::MergeCancel => {
                self.handle_merge_cancel(
                    &octocrab,
                    owner,
                    repo_name,
                    pr_number,
                    comment_id,
                    requester_id,
                    requester_login,
                )
                .await
            },
            CommentCommand::Merge { method } => {
                self.handle_merge_accept(
                    &octocrab,
                    owner,
                    repo_name,
                    pr_number,
                    comment_id,
                    requester_id,
                    requester_login,
                    method.as_ref().map(|m| m.as_str()),
                    comment_created_at,
                )
                .await
            },
        }
    }

    /// Outcome of an authorization check against a commenter.
    async fn authorize_commenter(
        &self,
        octocrab: &Octocrab,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        requester_id: i64,
        requester_login: &str,
    ) -> Result<Authorization> {
        let perm = match actions::check_repo_permission_for_user(
            octocrab,
            owner,
            repo_name,
            requester_login,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    "Failed to check repo permission for {} on {}/{}: {:?}",
                    requester_login, owner, repo_name, e
                );
                return Ok(Authorization::Abort);
            },
        };
        let has_write = matches!(
            perm,
            crate::auth::types::GitHubPermission::Admin
                | crate::auth::types::GitHubPermission::Maintain
                | crate::auth::types::GitHubPermission::Write
        );
        if has_write {
            return Ok(Authorization::Granted { has_write: true });
        }

        let changed = crate::db::github::get_pr_changed_packages(
            pr_number,
            owner,
            repo_name,
            &self.db_service.pool,
        )
        .await
        .unwrap_or_default();
        let is_pkg_maintainer = if changed.is_empty() {
            false
        } else {
            crate::db::maintainers::is_maintainer_of_all_packages(
                requester_id,
                &changed,
                &self.db_service.pool,
            )
            .await
            .unwrap_or(false)
        };

        if is_pkg_maintainer {
            Ok(Authorization::Granted { has_write: false })
        } else {
            Ok(Authorization::Denied)
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_merge_cancel(
        &self,
        octocrab: &Octocrab,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        comment_id: i64,
        requester_id: i64,
        requester_login: &str,
    ) -> Result<()> {
        // Silent no-op when nothing is pending so unauthorized commenters
        // learn no bot state.
        let pr_row = crate::db::github::get_pull_request_row(
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?;
        let Some(pending) = pr_row.as_ref().and_then(|p| p.pending_comment_merge()) else {
            debug!(
                "No pending comment-merge on {}/{}#{}; ignoring cancel from {}",
                owner, repo_name, pr_number, requester_login
            );
            return Ok(());
        };

        let is_self = requester_id == pending.requester_id;

        // Self-cancel fast path: skip API permission lookup.
        let authorized = if is_self {
            true
        } else {
            match self
                .authorize_commenter(
                    octocrab,
                    owner,
                    repo_name,
                    pr_number,
                    requester_id,
                    requester_login,
                )
                .await?
            {
                Authorization::Granted { .. } => true,
                Authorization::Denied => false,
                Authorization::Abort => return Ok(()),
            }
        };

        if !authorized {
            info!(
                "Denying @eka-ci merge cancel from {} on {}/{}#{}: not the original requester, no \
                 repo write, and not a maintainer of all changed packages",
                requester_login, owner, repo_name, pr_number
            );
            if let Err(e) =
                actions::add_comment_reaction(octocrab, owner, repo_name, comment_id, "-1").await
            {
                warn!("Failed to react to denied merge-cancel: {:?}", e);
            }
            let _ = actions::post_issue_comment(
                octocrab,
                owner,
                repo_name,
                pr_number,
                &format!(
                    "@{} I can't cancel this merge request — you must be the original requester, \
                     have write access to the repository, or be a maintainer of all affected \
                     packages.",
                    requester_login
                ),
            )
            .await;
            return Ok(());
        }

        // Authorized: clear the pending request and ack.
        crate::db::github::clear_comment_merge_request(
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?;
        if let Err(e) =
            actions::add_comment_reaction(octocrab, owner, repo_name, comment_id, "+1").await
        {
            warn!(
                "Failed to ack merge-cancel on comment {}: {:?}",
                comment_id, e
            );
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_merge_accept(
        &self,
        octocrab: &Octocrab,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        comment_id: i64,
        requester_id: i64,
        requester_login: &str,
        method_str: Option<&str>,
        comment_created_at: &chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        match self
            .authorize_commenter(
                octocrab,
                owner,
                repo_name,
                pr_number,
                requester_id,
                requester_login,
            )
            .await?
        {
            Authorization::Granted { .. } => {},
            Authorization::Abort => return Ok(()),
            Authorization::Denied => {
                info!(
                    "Denying @eka-ci merge from {} on {}/{}#{}: no repo write and not a \
                     maintainer of all changed packages",
                    requester_login, owner, repo_name, pr_number
                );
                if let Err(e) =
                    actions::add_comment_reaction(octocrab, owner, repo_name, comment_id, "-1")
                        .await
                {
                    warn!("Failed to react to denied merge: {:?}", e);
                }
                let _ = actions::post_issue_comment(
                    octocrab,
                    owner,
                    repo_name,
                    pr_number,
                    &format!(
                        "@{} I can't merge this PR — you need write access to the repository or \
                         be a maintainer of all affected packages.",
                        requester_login
                    ),
                )
                .await;
                return Ok(());
            },
        }

        // Pin the merge to the current head SHA.
        let Some(pr) = crate::db::github::get_pull_request_row(
            owner,
            repo_name,
            pr_number,
            &self.db_service.pool,
        )
        .await?
        else {
            warn!(
                "PR {}/{}#{} not found when processing merge command",
                owner, repo_name, pr_number
            );
            return Ok(());
        };

        if !self
            .check_push_timing(
                octocrab,
                owner,
                repo_name,
                pr_number,
                comment_id,
                requester_login,
                &pr.head_sha,
                comment_created_at,
            )
            .await?
        {
            return Ok(());
        }

        let rows = crate::db::github::set_comment_merge_request(
            owner,
            repo_name,
            pr_number,
            &pr.head_sha,
            method_str,
            requester_id,
            requester_login,
            comment_id,
            &self.db_service.pool,
        )
        .await?;
        if rows == 0 {
            warn!(
                "set_comment_merge_request affected 0 rows for {}/{}#{}",
                owner, repo_name, pr_number
            );
            return Ok(());
        }

        // Ack; the actual merge fires via the auto-merge evaluator.
        if let Err(e) =
            actions::add_comment_reaction(octocrab, owner, repo_name, comment_id, "+1").await
        {
            warn!("Failed to ack merge command: {:?}", e);
        }

        // Fire the evaluator in case gates are already green.
        let _ = self
            .github_sender
            .send(GitHubTask::CheckAutoMerge {
                owner: owner.to_string(),
                repo_name: repo_name.to_string(),
                pr_number,
            })
            .await;

        Ok(())
    }

    /// Best-effort force-push detection. Returns `Ok(true)` to proceed,
    /// `Ok(false)` to refuse (caller must return early). API errors and
    /// missing dates fall through to accept — the post-acceptance drift
    /// hook is the backstop. 30s grace is deliberately tight.
    #[allow(clippy::too_many_arguments)]
    async fn check_push_timing(
        &self,
        octocrab: &Octocrab,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        comment_id: i64,
        requester_login: &str,
        head_sha: &str,
        comment_created_at: &chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        const PUSH_GRACE: chrono::Duration = chrono::Duration::seconds(30);
        match actions::fetch_head_commit_date(octocrab, owner, repo_name, head_sha).await {
            Ok(Some(commit_date)) if commit_date > *comment_created_at + PUSH_GRACE => {
                info!(
                    "Refusing @eka-ci merge from {} on {}/{}#{}: head commit {} committed at {} \
                     is newer than the command comment at {} (grace={}s); likely post-command push",
                    requester_login,
                    owner,
                    repo_name,
                    pr_number,
                    head_sha,
                    commit_date,
                    comment_created_at,
                    PUSH_GRACE.num_seconds()
                );
                if let Err(e) =
                    actions::add_comment_reaction(octocrab, owner, repo_name, comment_id, "-1")
                        .await
                {
                    warn!("Failed to react to refused merge (push drift): {:?}", e);
                }
                let _ = actions::post_issue_comment(
                    octocrab,
                    owner,
                    repo_name,
                    pr_number,
                    &format!(
                        "@{} I can't merge this PR — the head commit (`{}`) appears to have been \
                         pushed after your `@eka-ci merge` command. Please review the latest \
                         changes and re-issue the command if you still want to merge.",
                        requester_login,
                        short_sha(head_sha)
                    ),
                )
                .await;
                Ok(false)
            },
            Ok(Some(_)) => Ok(true), // commit predates the comment
            Ok(None) => {
                warn!(
                    "Head commit {}@{}/{} has no parseable committer date; proceeding without \
                     push-time check (best effort)",
                    head_sha, owner, repo_name
                );
                Ok(true)
            },
            Err(e) => {
                warn!(
                    "Failed to fetch head commit date for {}/{}@{}: {:?}; proceeding without \
                     push-time check (best effort)",
                    owner, repo_name, head_sha, e
                );
                Ok(true)
            },
        }
    }
}

/// Result of the commenter authorization check.
enum Authorization {
    /// Commenter is authorized. `has_write` distinguishes the "repo
    /// write" path from the "package maintainer" path (kept for logging).
    Granted {
        #[allow(dead_code)]
        has_write: bool,
    },
    /// Commenter is not authorized. Caller should react `-1` + post
    /// explanation comment.
    Denied,
    /// Permission lookup failed; caller should drop the command silently.
    Abort,
}

/// Abbreviate a SHA to 7 chars; shorter inputs are returned unchanged.
fn short_sha(sha: &str) -> &str {
    if sha.len() >= 7 { &sha[..7] } else { sha }
}
