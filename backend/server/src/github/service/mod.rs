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
use crate::services::AsyncService;

/// Debounce window before the aggregated change-summary check is posted.
pub(crate) const CHANGE_SUMMARY_DEBOUNCE: Duration = Duration::from_secs(5 * 60);

// Sub-modules
pub mod actions;
mod auto_merge;
mod change_summary;
mod checks;
mod jobsets;
mod task_handler;
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
    github_receiver: Option<mpsc::Receiver<GitHubTask>>,
    github_sender: mpsc::Sender<GitHubTask>,
    github_configure_checks: Mutex<HashMap<Commit, CheckRunId>>,
    github_eval_checks: Mutex<HashMap<(Commit, String), CheckRunId>>,
    /// Tracks the change-summary check id per head SHA for idempotent updates.
    change_summary_checks: Mutex<HashMap<Commit, CheckRunId>>,
    /// Dedup guard so a single debounce timer fires per head SHA.
    change_summary_pending: Mutex<HashSet<Commit>>,
    /// Graph handle used to compute rebuild-impact for change summaries.
    graph_handle: GraphServiceHandle,
    /// Optional metrics for change-summary pipeline observability.
    change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
    /// Ingress sender for dispatching build requests after jobset diff.
    ingress_sender: Option<mpsc::Sender<IngressTask>>,
}

impl GitHubService {
    pub async fn new(
        db_service: DbService,
        octocrab: Octocrab,
        graph_handle: GraphServiceHandle,
        change_summary_metrics: Option<Arc<ChangeSummaryMetrics>>,
        ingress_sender: Option<mpsc::Sender<IngressTask>>,
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

        let (github_sender, github_receiver) = mpsc::channel(10_000);
        Ok(Self {
            db_service,
            octocrab,
            installations,
            github_receiver: Some(github_receiver),
            github_sender,
            github_configure_checks: Mutex::new(HashMap::new()),
            github_eval_checks: Mutex::new(HashMap::new()),
            change_summary_checks: Mutex::new(HashMap::new()),
            change_summary_pending: Mutex::new(HashSet::new()),
            graph_handle,
            change_summary_metrics,
            ingress_sender,
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

    async fn handle_task(&self, task: GitHubTask) -> Result<()> {
        self.handle_github_task(&task).await
    }

    async fn handle_failure(&mut self, error: anyhow::Error) {
        error!("GitHubService task failed: {:?}", error);
    }

    async fn handle_closure(&mut self) {
        info!("GitHubService shutting down");
    }
}
