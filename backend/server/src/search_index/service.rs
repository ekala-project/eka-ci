// SearchIndexService: generates and uploads a SQLite search database
// after channel promotion.
//
// This is a lightweight AsyncService that receives GenerateIndexes
// tasks from the ChannelService. It runs generators to collect
// package and file entries, builds a single SQLite database with
// FTS5 indexes, and uploads the result.
// Uploads are namespaced by channel name so multiple channels coexist.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{Context, Result};
use sqlx::SqlitePool;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::types::{Manifest, SearchIndexTask};
use super::{generators, upload};
use crate::config::{ChannelConfig, ChannelForge, SearchIndexConfig};
use crate::services::{AsyncService, TaskJournal};

const SEARCH_INDEX_TASK_BUFFER: usize = 100;

pub struct SearchIndexService {
    task_sender: mpsc::Sender<SearchIndexTask>,
    task_receiver: Option<mpsc::Receiver<SearchIndexTask>>,
    config: SearchIndexConfig,
    pool: SqlitePool,
    journal: TaskJournal<SearchIndexTask>,
}

impl SearchIndexService {
    pub fn new(config: SearchIndexConfig, pool: SqlitePool) -> Self {
        let (task_sender, task_receiver) = mpsc::channel(SEARCH_INDEX_TASK_BUFFER);
        Self {
            task_sender,
            task_receiver: Some(task_receiver),
            config,
            journal: TaskJournal::new(pool.clone(), "search_index"),
            pool,
        }
    }

    /// Handle a GenerateIndexes task: run generators, build a SQLite
    /// database, and upload the single `search.db` file.
    async fn handle_generate(
        &self,
        channel_id: &str,
        channel: &ChannelConfig,
        sha: &str,
    ) -> Result<()> {
        let start = Instant::now();
        info!(
            event = "search_index_generation_start",
            channel_id = %channel_id,
            channel_name = %channel.name,
            sha = %sha,
            "starting search index generation"
        );

        let flake_ref = build_flake_ref(channel, sha);
        let system = "x86_64-linux";

        // Generate package entries.
        let packages =
            match generators::packages::generate_package_entries(&flake_ref, system).await {
                Ok(entries) => entries,
                Err(e) => {
                    warn!(
                        event = "search_index_packages_failed",
                        error = %e,
                        "failed to generate package entries; building db without packages"
                    );
                    Vec::new()
                },
            };

        // Collect store outputs from successful builds for file indexing.
        let store_outputs =
            query_successful_outputs(&self.pool, sha, &channel.owner, &channel.repo).await;
        if store_outputs.is_empty() {
            info!(
                event = "search_index_no_store_outputs",
                sha = %sha,
                "no successful build outputs found; file index will be empty"
            );
        } else {
            info!(
                event = "search_index_store_outputs_collected",
                packages = store_outputs.len(),
                "collected store outputs for file indexing"
            );
        }

        // Generate file entries (walks store paths).
        let files = match generators::files::generate_file_entries(&store_outputs).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!(
                    event = "search_index_files_failed",
                    error = %e,
                    "failed to generate file entries; building db without files"
                );
                Vec::new()
            },
        };

        let manifest = Manifest {
            generated_at: chrono::Utc::now().to_rfc3339(),
            channel_name: channel.name.clone(),
            nixpkgs_rev: sha.to_string(),
            package_count: packages.len(),
            file_count: files.len(),
            option_count: 0,
        };

        // Build the SQLite database (blocking I/O, run on spawn_blocking).
        let db_data = {
            let pkgs = packages;
            let fls = files;
            let m = manifest.clone();
            tokio::task::spawn_blocking(move || {
                generators::sqlite::build_database(&pkgs, &fls, &[], &m)
            })
            .await??
        };

        // Upload the database.
        if let Err(e) = upload::upload_database(&self.config, &channel.name, &db_data).await {
            warn!(
                event = "search_index_upload_failed",
                error = %e,
                "failed to upload search.db"
            );
            return Err(e);
        }

        info!(
            event = "search_index_generation_complete",
            channel_id = %channel_id,
            channel_name = %channel.name,
            sha = %sha,
            elapsed_ms = start.elapsed().as_millis() as u64,
            db_bytes = db_data.len(),
            packages = manifest.package_count,
            files = manifest.file_count,
            "search index generation complete"
        );

        Ok(())
    }

    /// Check whether this channel should trigger index generation.
    fn should_generate(&self, channel_id: &str) -> bool {
        if self.config.channels.is_empty() {
            return true;
        }
        self.config.channels.iter().any(|c| c == channel_id)
    }
}

/// Build a Nix flake reference from a channel config and commit SHA.
fn build_flake_ref(channel: &ChannelConfig, sha: &str) -> String {
    match &channel.forge {
        ChannelForge::GitHub => {
            format!("github:{}/{}/{}", channel.owner, channel.repo, sha)
        },
        ChannelForge::GitLab { domain } => {
            format!(
                "git+https://{}/{}/{}?rev={}",
                domain, channel.owner, channel.repo, sha
            )
        },
        ChannelForge::Gitea { domain } => {
            format!(
                "git+https://{}/{}/{}?rev={}",
                domain, channel.owner, channel.repo, sha
            )
        },
    }
}

/// Query the database for store output paths of successfully-built
/// derivations in all jobsets for a given SHA and repository.
///
/// Returns a map of `attr_name → [(output_name, store_path)]` suitable
/// for passing to `generate_file_entries`.
async fn query_successful_outputs(
    pool: &SqlitePool,
    sha: &str,
    owner: &str,
    repo: &str,
) -> HashMap<String, Vec<(String, String)>> {
    let rows: Vec<(String, String, String)> = match sqlx::query_as(
        r#"
        SELECT j.name, r.output_name, r.output_path
        FROM Job j
        INNER JOIN GitHubJobSets js ON j.jobset = js.ROWID
        INNER JOIN Drv d ON j.drv_id = d.ROWID
        INNER JOIN DrvRuntimeRefs r ON r.drv_id = d.ROWID
        WHERE js.sha = ?
          AND js.owner = ?
          AND js.repo_name = ?
          AND d.build_state = 42
        GROUP BY j.name, r.output_name, r.output_path
        "#,
    )
    .bind(sha)
    .bind(owner)
    .bind(repo)
    .fetch_all(pool)
    .await
    .context("failed to query successful build outputs")
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(
                event = "search_index_output_query_failed",
                error = %e,
                "failed to query build outputs; file index will be empty"
            );
            return HashMap::new();
        },
    };

    let mut outputs: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (attr, output_name, output_path) in rows {
        outputs
            .entry(attr)
            .or_default()
            .push((output_name, output_path));
    }
    outputs
}

impl AsyncService<SearchIndexTask> for SearchIndexService {
    fn get_sender(&self) -> mpsc::Sender<SearchIndexTask> {
        self.task_sender.clone()
    }

    fn take_receiver(&mut self) -> Option<mpsc::Receiver<SearchIndexTask>> {
        self.task_receiver.take()
    }

    fn task_journal(&self) -> Option<&TaskJournal<SearchIndexTask>> {
        Some(&self.journal)
    }

    async fn handle_task(&self, task: SearchIndexTask) -> Result<()> {
        match task {
            SearchIndexTask::GenerateIndexes {
                channel_id,
                channel,
                sha,
            } => {
                if !self.should_generate(&channel_id) {
                    info!(
                        event = "search_index_skip_channel",
                        channel_id = %channel_id,
                        "channel not in search_index.channels list; skipping"
                    );
                    return Ok(());
                }
                self.handle_generate(&channel_id, &channel, &sha).await
            },
        }
    }

    async fn handle_failure(&mut self, error: anyhow::Error) {
        warn!(
            event = "search_index_service_task_failed",
            error = ?error,
            "SearchIndexService task failed; continuing to next task"
        );
    }

    async fn handle_closure(&mut self) {
        info!(
            event = "search_index_service_shutdown",
            "SearchIndexService shutdown requested; draining"
        );
    }
}
