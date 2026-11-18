// GitHub service orchestration and task processing

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use octocrab::Octocrab;
use octocrab::models::{CheckRunId, Installation};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};

use crate::db::DbService;
use crate::graph::GraphServiceHandle;
use crate::metrics::ChangeSummaryMetrics;
use crate::scheduler::IngressTask;
use crate::services::{AsyncService, TaskJournal};

/// Debounce window before the aggregated change-summary check is posted.
pub(crate) const CHANGE_SUMMARY_DEBOUNCE: Duration = Duration::from_secs(5 * 60);

// Sub-modules
pub mod actions;
mod auto_merge;
pub mod build_report;
mod change_summary;
mod checks;
mod concluded;
mod gate_kind;
pub mod graphql_batch;
mod jobsets;
mod passthru_filter;
mod size_warning;
mod task_handler;
mod types;

pub(crate) use concluded::request_auto_merge_check;
pub use types::{CICheckInfo, Commit, GitHubTask, JobDifference, Owner};

/// This service will be response for pushing/posting events to GitHub
pub struct GitHubService {
    // Channels to individual services
    // We may in the future need to recover an individual service, so retaining
    // a handle to the other service channels will be prequisite
    db_service: DbService,
    octocrab: Octocrab,
    installations: HashMap<Owner, Installation>,
    github_receiver: Option<mpsc::Receiver<GitHubTask>>,
    github_sender: mpsc::Sender<GitHubTask>,
    github_configure_checks: Mutex<HashMap<Commit, CheckRunId>>,
    github_eval_checks: Mutex<HashMap<(Commit, String), CheckRunId>>,
    /// Dedup guard so a single debounce timer fires per head SHA.
    change_summary_pending: Mutex<HashSet<Commit>>,
    /// Graph handle used to compute rebuild-impact for change summaries.
    graph_handle: GraphServiceHandle,
    /// Computes and posts change-summary checks in the background.
    change_summary: change_summary::ChangeSummaryRunner,
    /// Ingress sender for dispatching build requests after jobset diff.
    ingress_sender: Option<mpsc::Sender<IngressTask>>,
    /// Eval sender for dispatching passthru.tests evaluations.
    eval_sender: Option<mpsc::Sender<crate::nix::EvalTask>>,
    /// Rate limiter to avoid flooding the GitHub API endpoint.
    rate_limiter: ApiRateLimiter,
    /// Batches check run updates for GraphQL flush (separate rate limit budget).
    pub(crate) batcher: Arc<graphql_batch::CheckRunBatcher>,
    journal: TaskJournal<GitHubTask>,
}

/// Upper bound on a single GitHub task. Generous: some tasks (jobset
/// creation, check-run resyncs) issue many rate-limited API calls.
const GITHUB_TASK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// Rate limiter that enforces a minimum interval between API calls
/// and provides retry-with-backoff for 429 responses.
pub(crate) struct ApiRateLimiter {
    min_interval: Duration,
    last_call: Mutex<tokio::time::Instant>,
}

impl ApiRateLimiter {
    fn new(requests_per_second: f64) -> Self {
        let min_interval = Duration::from_secs_f64(1.0 / requests_per_second);
        Self {
            min_interval,
            last_call: Mutex::new(tokio::time::Instant::now()),
        }
    }

    /// Wait until enough time has passed since the last call.
    async fn acquire(&self) {
        let mut last = self.last_call.lock().await;
        let elapsed = last.elapsed();
        if elapsed < self.min_interval {
            tokio::time::sleep(self.min_interval - elapsed).await;
        }
        *last = tokio::time::Instant::now();
    }

    /// Back off after a 429 response. Doubles the minimum interval
    /// (up to 30s) and sleeps for the backoff duration.
    async fn backoff_429(&self) {
        warn!("GitHub API rate limit hit (429), backing off");
        // Sleep for 60s on rate limit — GitHub's secondary rate
        // limit resets within this window.
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

/// Check if an octocrab error is a 429 rate limit response.
pub(crate) fn is_rate_limited(err: &anyhow::Error) -> bool {
    if let Some(octocrab::Error::GitHub { source, .. }) = err.downcast_ref::<octocrab::Error>() {
        return source.status_code == http::StatusCode::TOO_MANY_REQUESTS;
    }
    false
}

impl GitHubService {
    pub async fn new(
        db_service: DbService,
        octocrab: Octocrab,
        graph_handle: GraphServiceHandle,
        change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
        ingress_sender: Option<mpsc::Sender<IngressTask>>,
        eval_sender: Option<mpsc::Sender<crate::nix::EvalTask>>,
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

        let pool = db_service.pool.clone();
        let change_summary = change_summary::ChangeSummaryRunner::new(
            db_service.clone(),
            graph_handle.clone(),
            change_summary_metrics,
        );
        let (github_sender, github_receiver) = mpsc::channel(1_000);
        Ok(Self {
            db_service,
            octocrab,
            installations,
            github_receiver: Some(github_receiver),
            github_sender,
            github_configure_checks: Mutex::new(HashMap::new()),
            github_eval_checks: Mutex::new(HashMap::new()),
            change_summary_pending: Mutex::new(HashSet::new()),
            change_summary,
            graph_handle,
            ingress_sender,
            eval_sender,
            // ~10 req/sec keeps well under GitHub's 5000/hr app limit
            // while still being responsive for check_run updates.
            rate_limiter: ApiRateLimiter::new(10.0),
            batcher: Arc::new(graphql_batch::CheckRunBatcher::new()),
            journal: TaskJournal::new(pool, "github"),
        })
    }

    pub fn get_sender(&self) -> mpsc::Sender<GitHubTask> {
        self.github_sender.clone()
    }

    #[allow(dead_code)] // Called via AsyncService trait dispatch
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<GitHubTask>> {
        self.github_receiver.take()
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
}

// ============================================================================
// AsyncService trait implementation
// ============================================================================

impl AsyncService<GitHubTask> for GitHubService {
    fn get_sender(&self) -> mpsc::Sender<GitHubTask> {
        self.github_sender.clone()
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<GitHubTask>> {
        self.github_receiver.take()
    }

    fn task_journal(&self) -> Option<&TaskJournal<GitHubTask>> {
        Some(&self.journal)
    }

    /// Override the default run() to spawn a GraphQL batch flush loop
    /// alongside the task processing loop.
    fn run(
        mut self,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let receiver = self.take_receiver().expect("receiver was already taken");

            // Spawn the batcher flush loop
            let batcher = self.batcher.clone();
            let flush_cancel = cancel_token.clone();
            let installations = self.installations.clone();
            let octocrab = self.octocrab.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(5));
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    tokio::select! {
                        _ = flush_cancel.cancelled() => break,
                        _ = interval.tick() => {
                            let insts = installations.clone();
                            let octo = octocrab.clone();
                            batcher
                                .flush(|owner| {
                                    let inst = insts
                                        .get(owner)
                                        .context("No installation for owner")?;
                                    octo.installation(inst.id)
                                        .map_err(anyhow::Error::from)
                                })
                                .await;
                        }
                    }
                }
            });

            self.serve(receiver, cancel_token).await;
        })
    }

    async fn handle_task(&self, task: GitHubTask) -> Result<()> {
        // The service is serial, so one hung task would stall every PR.
        // Bound it; a timed-out task stays journaled and is replayed (a
        // bounded number of times) on the next start.
        tokio::time::timeout(GITHUB_TASK_TIMEOUT, self.handle_task_with_retry(&task))
            .await
            .with_context(|| {
                format!(
                    "GitHub task {} timed out after {GITHUB_TASK_TIMEOUT:?}",
                    task_variant_name(&task)
                )
            })?
    }

    async fn handle_failure(&mut self, error: anyhow::Error) {
        error!("GitHubService task failed: {:?}", error);
    }

    async fn handle_closure(&mut self) {
        info!("GitHubService shutting down");
    }
}

impl GitHubService {
    async fn handle_task_with_retry(&self, task: &GitHubTask) -> Result<()> {
        match self.handle_github_task(task).await {
            Ok(()) => Ok(()),
            Err(e) if is_rate_limited(&e) => {
                self.rate_limiter.backoff_429().await;
                // Retry once after backoff
                self.handle_github_task(task).await
            },
            Err(e) => Err(e),
        }
    }
}

/// The enum variant of `task` (e.g. `CreateJobSet`), for log messages
/// where the full `Debug` output would be too large.
fn task_variant_name(task: &GitHubTask) -> String {
    let debug = format!("{task:?}");
    debug
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .next()
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_variant_name_strips_fields() {
        let task = GitHubTask::CreateChangeSummaryCheck {
            ci_check_info: Arc::new(CICheckInfo {
                commit: "abc".into(),
                base_commit: None,
                owner: "o".into(),
                repo_name: "r".into(),
                branch: None,
            }),
            job: "core-pkgs".into(),
        };
        assert_eq!(task_variant_name(&task), "CreateChangeSummaryCheck");
    }
}
