// Jobset and build management

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;
use tracing::{debug, warn};

use super::GitHubService;
use crate::db::github::NewOrChangedJob;
use crate::db::model::DrvId;
use crate::db::model::build_event::{
    DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState,
};
use crate::github::service::types::JobDifference;
use crate::github::service::{CHANGE_SUMMARY_DEBOUNCE, CICheckInfo, GitHubTask, actions};
use crate::nix::NixEvalDrv;

impl GitHubService {
    /// Threshold for per-package check_run creation. When the number of
    /// new or changed packages is below this value, individual check_runs
    /// are created eagerly so each package is visible as a separate CI
    /// gate. At or above this threshold, individual check_runs are
    /// omitted (the eval gate serves as the summary) and only build
    /// failures produce per-package check_runs lazily.
    const EAGER_CHECK_RUN_THRESHOLD: usize = 500;

    pub(super) async fn create_job_set(
        &self,
        ci_check_info: &std::sync::Arc<CICheckInfo>,
        name: &str,
        jobs: &[NixEvalDrv],
        config_json: Option<&str>,
    ) -> Result<()> {
        // Pass base commit SHA for incremental eval optimization
        // This allows job differences to be computed during insertion
        let base_sha = ci_check_info.base_commit.as_deref();

        let jobset_id = self
            .db_service
            .create_github_jobset_with_jobs(
                &ci_check_info.commit,
                name,
                &ci_check_info.owner,
                &ci_check_info.repo_name,
                jobs,
                config_json,
                base_sha,
            )
            .await?;

        // This is only relevant on PRs, missing a base commit denotes that
        // this jobset creation is done for a base_commit
        if let Some(base_commit) = ci_check_info.base_commit.as_ref() {
            // Create per-package check_runs and dispatch builds for
            // new/changed packages.
            self.create_eager_check_runs_and_dispatch_builds(ci_check_info, name, jobset_id)
                .await?;

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
                .lock()
                .await
                .insert(ci_check_info.commit.clone())
            {
                self.spawn_change_summary_debounce(Arc::clone(ci_check_info), name.to_string());
            }
        }
        Ok(())
    }

    /// Create per-package check_runs for new/changed packages and
    /// dispatch build requests to the ingress service.
    ///
    /// Check_runs are created eagerly when the count is below
    /// `EAGER_CHECK_RUN_THRESHOLD`. For larger sets, the eval gate
    /// serves as the summary and only build failures produce
    /// per-package check_runs lazily.
    ///
    /// Build requests are always dispatched for all new/changed
    /// packages regardless of the check_run threshold.
    async fn create_eager_check_runs_and_dispatch_builds(
        &self,
        ci_check_info: &std::sync::Arc<CICheckInfo>,
        job_name: &str,
        jobset_id: i64,
    ) -> Result<()> {
        let changed_jobs = self.db_service.get_new_or_changed_jobs(jobset_id).await?;

        // Count effective gates (coalesced groups count as 1 each)
        let (groups, ungrouped) = group_jobs_by_prefix(&changed_jobs);
        let effective_gate_count = groups.len() + ungrouped.len();
        // Drop these — create_coalesced_check_runs will re-partition
        drop(groups);
        drop(ungrouped);

        // Create per-package check_runs only below the threshold
        if !changed_jobs.is_empty() && effective_gate_count < Self::EAGER_CHECK_RUN_THRESHOLD {
            debug!(
                "Creating eager check_runs for {} packages ({} effective gates) in jobset {}",
                changed_jobs.len(),
                effective_gate_count,
                job_name,
            );

            self.create_coalesced_check_runs(ci_check_info, job_name, &changed_jobs)
                .await?;
        } else if effective_gate_count >= Self::EAGER_CHECK_RUN_THRESHOLD {
            debug!(
                "Skipping eager check_run creation for {} packages ({} effective gates, threshold {})",
                changed_jobs.len(),
                effective_gate_count,
                Self::EAGER_CHECK_RUN_THRESHOLD,
            );
        }

        // Dispatch build requests for all new/changed packages
        if let Some(ingress_sender) = &self.ingress_sender {
            debug!(
                "Dispatching {} ingress EvalRequests for new/changed packages",
                changed_jobs.len(),
            );
            let drv_ids: Vec<_> = changed_jobs
                .iter()
                .map(|j| std::sync::Arc::new(j.drv_path.clone()))
                .collect();

            // First dispatch deps of changed packages for substitution
            // checking. These were inserted into the graph by batch_traverse
            // but their state is still Queued — the ingress needs to check
            // if they're cached so is_buildable works for the changed packages.
            let mut dispatched_deps = std::collections::HashSet::new();
            for drv_id in &drv_ids {
                let shared_id = match crate::graph_compat::to_shared_drv_id(drv_id) {
                    Ok(id) => id,
                    Err(_) => continue,
                };
                if let Some(node) = self.graph_handle.get_node(&shared_id) {
                    for dep in node.dependencies.iter() {
                        if dispatched_deps.insert(dep.clone()) {
                            if let Ok(server_dep) = crate::graph_compat::to_server_drv_id(dep) {
                                let _ = ingress_sender
                                    .send(crate::scheduler::IngressTask::EvalRequest(
                                        std::sync::Arc::new(server_dep),
                                    ))
                                    .await;
                            }
                        }
                    }
                }
            }
            debug!(
                "Dispatched {} unique dep EvalRequests for changed packages",
                dispatched_deps.len(),
            );

            // Then dispatch the changed packages themselves
            for drv_id in &drv_ids {
                if let Err(e) = ingress_sender
                    .send(crate::scheduler::IngressTask::EvalRequest(
                        std::sync::Arc::clone(drv_id),
                    ))
                    .await
                {
                    warn!(
                        "Failed to send IngressTask for {}: {:?}",
                        drv_id.store_path(),
                        e
                    );
                    break;
                }
            }

            // Periodically re-dispatch changed packages to catch
            // those whose deps complete progressively through builds.
            // Each round re-runs dry_run_realise which detects newly
            // available deps from completed builds.
            let sender = ingress_sender.clone();
            crate::services::spawn_logged("re-dispatch-loop", async move {
                for round in 0..30 {
                    tokio::time::sleep(std::time::Duration::from_secs(180)).await;
                    let mut any_queued = false;
                    for drv_id in &drv_ids {
                        // EvalRequest re-runs dry_run_realise which
                        // picks up newly available deps from builds
                        if sender
                            .send(crate::scheduler::IngressTask::EvalRequest(
                                std::sync::Arc::clone(drv_id),
                            ))
                            .await
                            .is_ok()
                        {
                            any_queued = true;
                        }
                    }
                    if !any_queued {
                        break;
                    }
                    tracing::debug!(
                        "Re-dispatch round {} for {} changed packages",
                        round + 1,
                        drv_ids.len()
                    );
                }
            });
        }

        Ok(())
    }

    /// Spawn the debounce timer that enqueues a `CreateChangeSummaryCheck`.
    pub(super) fn spawn_change_summary_debounce(
        &self,
        ci_check_info: Arc<CICheckInfo>,
        job: String,
    ) {
        let sender = self.github_sender.clone();
        crate::services::spawn_logged("change-summary-debounce", async move {
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

    /// Create per-package check_runs for new/changed packages, coalescing
    /// dotted attr paths (e.g., `linux.v6_17`, `linux.v6_18`) into a single
    /// parent gate (`linux`). Non-dotted attrs get individual check_runs.
    async fn create_coalesced_check_runs(
        &self,
        ci_check_info: &Arc<CICheckInfo>,
        job_name: &str,
        changed_jobs: &[NewOrChangedJob],
    ) -> Result<()> {
        let (groups, ungrouped) = group_jobs_by_prefix(changed_jobs);

        let octocrab = self.octocrab_for_owner(&ci_check_info.owner)?;

        // Create individual check_runs for ungrouped (non-dotted) jobs
        for job in &ungrouped {
            let check_run = ci_check_info
                .create_gh_check_run(
                    &octocrab,
                    job_name,
                    &job.name,
                    job.build_state.clone(),
                    &job.difference,
                )
                .await?;

            crate::db::github::insert_check_run_info_with_node_id(
                check_run.id.0 as i64,
                &job.drv_path,
                &ci_check_info.repo_name,
                &ci_check_info.owner,
                Some(&check_run.node_id),
                &self.db_service.pool,
            )
            .await?;
        }

        // Create coalesced check_runs for groups
        for group in &groups {
            let summary = build_variant_summary(&group.variants);
            let check_run = ci_check_info
                .create_coalesced_gh_check_run(
                    &octocrab,
                    job_name,
                    &group.group_name,
                    group.worst_build_state.clone(),
                    &group.worst_difference,
                    &summary,
                )
                .await?;

            // Insert one GitHubCheckRuns row per variant, all sharing the
            // same check_run_id so UpdateBuildStatus can find the gate.
            for variant in &group.variants {
                crate::db::github::insert_check_run_info_with_node_id(
                    check_run.id.0 as i64,
                    &variant.drv_path,
                    &ci_check_info.repo_name,
                    &ci_check_info.owner,
                    Some(&check_run.node_id),
                    &self.db_service.pool,
                )
                .await?;
            }
        }

        debug!(
            "Created {} coalesced gates and {} individual check_runs for jobset {}",
            groups.len(),
            ungrouped.len(),
            job_name,
        );

        Ok(())
    }

    pub(super) async fn handle_update_build_status_with_size_warning(
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
}

// ---------------------------------------------------------------------------
// Variant grouping and aggregation helpers
// ---------------------------------------------------------------------------

/// A group of jobs sharing the same dotted prefix (e.g., `linux.v6_17` and
/// `linux.v6_18` both belong to the `linux` group).
pub(crate) struct VariantGroup<'a> {
    pub group_name: String,
    pub variants: Vec<&'a NewOrChangedJob>,
    pub worst_difference: JobDifference,
    pub worst_build_state: DrvBuildState,
}

/// Partition jobs into coalesced groups (dotted attr paths) and ungrouped
/// (non-dotted) jobs. Groups are sorted by name for deterministic ordering.
pub(crate) fn group_jobs_by_prefix(
    jobs: &[NewOrChangedJob],
) -> (Vec<VariantGroup<'_>>, Vec<&NewOrChangedJob>) {
    let mut groups: BTreeMap<String, Vec<&NewOrChangedJob>> = BTreeMap::new();
    let mut ungrouped: Vec<&NewOrChangedJob> = Vec::new();

    for job in jobs {
        if let Some(dot_pos) = job.name.find('.') {
            let prefix = &job.name[..dot_pos];
            groups.entry(prefix.to_string()).or_default().push(job);
        } else {
            ungrouped.push(job);
        }
    }

    let variant_groups = groups
        .into_iter()
        .map(|(group_name, variants)| {
            let worst_difference = variants
                .iter()
                .map(|v| &v.difference)
                .fold(JobDifference::Removed, |acc, d| worst_difference(&acc, d));
            let worst_build_state = aggregate_build_state(
                &variants
                    .iter()
                    .map(|v| v.build_state.clone())
                    .collect::<Vec<_>>(),
            );
            VariantGroup {
                group_name,
                variants,
                worst_difference,
                worst_build_state,
            }
        })
        .collect();

    (variant_groups, ungrouped)
}

/// Return the more severe of two `JobDifference` values (public entry point).
/// Priority: Changed > New > Removed.
pub(crate) fn worst_difference_pub(a: &JobDifference, b: &JobDifference) -> JobDifference {
    worst_difference(a, b)
}

/// Return the more severe of two `JobDifference` values.
/// Priority: Changed > New > Removed.
fn worst_difference(a: &JobDifference, b: &JobDifference) -> JobDifference {
    fn severity(d: &JobDifference) -> u8 {
        match d {
            JobDifference::Removed => 0,
            JobDifference::New => 1,
            JobDifference::Changed => 2,
        }
    }
    if severity(a) >= severity(b) {
        a.clone()
    } else {
        b.clone()
    }
}

/// Compute a severity score for a build state. Higher = worse.
fn build_state_severity(state: &DrvBuildState) -> u8 {
    match state {
        DrvBuildState::Completed(DrvBuildResult::Success) => 0,
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => 1,
        DrvBuildState::Queued => 2,
        DrvBuildState::Buildable => 2,
        DrvBuildState::Blocked => 2,
        DrvBuildState::FailedRetry => 3,
        DrvBuildState::Building => 3,
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout) => 4,
        DrvBuildState::Completed(DrvBuildResult::Failure) => 5,
        DrvBuildState::TransitiveFailure => 5,
        DrvBuildState::UnsatisfiableRequirements => 5,
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::OutOfMemory) => 5,
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath) => 5,
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::SchedulerDeath) => 5,
    }
}

/// Compute the worst-case aggregate across a set of build states.
pub(crate) fn aggregate_build_state(states: &[DrvBuildState]) -> DrvBuildState {
    states
        .iter()
        .max_by_key(|s| build_state_severity(s))
        .cloned()
        .unwrap_or(DrvBuildState::Queued)
}

/// Render a status emoji for a build state.
fn state_emoji(state: &DrvBuildState) -> &'static str {
    match state {
        DrvBuildState::Completed(DrvBuildResult::Success) => ":white_check_mark:",
        DrvBuildState::Completed(DrvBuildResult::Failure) => ":x:",
        DrvBuildState::TransitiveFailure => ":x:",
        DrvBuildState::UnsatisfiableRequirements => ":x:",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout) => ":alarm_clock:",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => ":no_entry_sign:",
        DrvBuildState::Interrupted(_) => ":x:",
        DrvBuildState::Building | DrvBuildState::FailedRetry => ":hourglass:",
        DrvBuildState::Queued | DrvBuildState::Buildable => ":clock3:",
        DrvBuildState::Blocked => ":lock:",
    }
}

/// Render a human-readable label for a build state.
fn state_label(state: &DrvBuildState) -> &'static str {
    match state {
        DrvBuildState::Completed(DrvBuildResult::Success) => "Success",
        DrvBuildState::Completed(DrvBuildResult::Failure) => "Failed",
        DrvBuildState::TransitiveFailure => "Transitive failure",
        DrvBuildState::UnsatisfiableRequirements => "Unsatisfiable requirements",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout) => "Timed out",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => "Cancelled",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::OutOfMemory) => "Out of memory",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::ProcessDeath) => "Process died",
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::SchedulerDeath) => "Scheduler died",
        DrvBuildState::Building => "Building",
        DrvBuildState::FailedRetry => "Retrying",
        DrvBuildState::Queued => "Queued",
        DrvBuildState::Buildable => "Queued",
        DrvBuildState::Blocked => "Blocked",
    }
}

/// Extract the variant suffix from a dotted attr path.
/// E.g., `linux.v6_17` → `v6_17`, `python.pkgs.setuptools` → `pkgs.setuptools`.
fn variant_suffix(attr_name: &str) -> &str {
    attr_name
        .find('.')
        .map(|i| &attr_name[i + 1..])
        .unwrap_or(attr_name)
}

/// Build a markdown summary table of variant statuses for the check run output.
/// Public entry point for the lazy failure path which has owned `NewOrChangedJob` refs.
pub(crate) fn build_variant_summary_for_new_or_changed(
    variants: &[&NewOrChangedJob],
) -> String {
    build_variant_summary(variants)
}

/// Build a markdown summary table of variant statuses for the check run output.
fn build_variant_summary(variants: &[&NewOrChangedJob]) -> String {
    let mut md = String::from("## Variants\n\n| Variant | Status |\n|---------|--------|\n");
    for v in variants {
        let suffix = variant_suffix(&v.name);
        let emoji = state_emoji(&v.build_state);
        let label = state_label(&v.build_state);
        md.push_str(&format!("| {} | {} {} |\n", suffix, emoji, label));
    }
    md
}

/// Build a markdown summary table from DB-fetched variant states.
/// Used when updating a coalesced gate on build status change.
pub(crate) fn build_variant_summary_from_states(
    variants: &[crate::db::github::VariantBuildState],
) -> String {
    let mut md = String::from("## Variants\n\n| Variant | Status |\n|---------|--------|\n");
    for v in variants {
        let suffix = variant_suffix(&v.name);
        let emoji = state_emoji(&v.build_state);
        let label = state_label(&v.build_state);
        md.push_str(&format!("| {} | {} {} |\n", suffix, emoji, label));
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::model::DrvId;

    fn make_job(name: &str, difference: JobDifference, state: DrvBuildState) -> NewOrChangedJob {
        NewOrChangedJob {
            name: name.to_string(),
            difference,
            drv_path: DrvId::dummy(),
            build_state: state,
        }
    }

    #[test]
    fn test_group_jobs_by_prefix_basic() {
        let jobs = vec![
            make_job("linux.v6_17", JobDifference::Changed, DrvBuildState::Queued),
            make_job("linux.v6_18", JobDifference::New, DrvBuildState::Queued),
            make_job("hello", JobDifference::Changed, DrvBuildState::Queued),
            make_job(
                "python.pkgs.setuptools",
                JobDifference::Changed,
                DrvBuildState::Queued,
            ),
            make_job(
                "python.pkgs.requests",
                JobDifference::New,
                DrvBuildState::Queued,
            ),
        ];

        let (groups, ungrouped) = group_jobs_by_prefix(&jobs);

        assert_eq!(ungrouped.len(), 1);
        assert_eq!(ungrouped[0].name, "hello");

        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].group_name, "linux");
        assert_eq!(groups[0].variants.len(), 2);
        assert_eq!(groups[1].group_name, "python");
        assert_eq!(groups[1].variants.len(), 2);
    }

    #[test]
    fn test_group_worst_difference() {
        let jobs = vec![
            make_job("linux.v6_17", JobDifference::Removed, DrvBuildState::Queued),
            make_job("linux.v6_18", JobDifference::New, DrvBuildState::Queued),
            make_job("linux.v6_19", JobDifference::Changed, DrvBuildState::Queued),
        ];

        let (groups, _) = group_jobs_by_prefix(&jobs);
        assert_eq!(groups[0].worst_difference, JobDifference::Changed);
    }

    #[test]
    fn test_aggregate_build_state_worst_wins() {
        use DrvBuildState::*;

        assert_eq!(
            aggregate_build_state(&[
                Completed(DrvBuildResult::Success),
                Building,
                Queued,
            ]),
            Building,
        );

        assert_eq!(
            aggregate_build_state(&[
                Completed(DrvBuildResult::Success),
                Completed(DrvBuildResult::Failure),
                Building,
            ]),
            Completed(DrvBuildResult::Failure),
        );

        assert_eq!(
            aggregate_build_state(&[
                Completed(DrvBuildResult::Success),
                Completed(DrvBuildResult::Success),
            ]),
            Completed(DrvBuildResult::Success),
        );
    }

    #[test]
    fn test_variant_suffix() {
        assert_eq!(variant_suffix("linux.v6_17"), "v6_17");
        assert_eq!(variant_suffix("python.pkgs.setuptools"), "pkgs.setuptools");
        assert_eq!(variant_suffix("hello"), "hello");
    }

    #[test]
    fn test_no_dotted_jobs_returns_empty_groups() {
        let jobs = vec![
            make_job("hello", JobDifference::New, DrvBuildState::Queued),
            make_job("cmake", JobDifference::Changed, DrvBuildState::Building),
        ];

        let (groups, ungrouped) = group_jobs_by_prefix(&jobs);
        assert!(groups.is_empty());
        assert_eq!(ungrouped.len(), 2);
    }
}
