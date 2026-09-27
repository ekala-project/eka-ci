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

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::types::{Manifest, SearchIndexTask};
use super::{generators, upload};
use crate::config::SearchIndexConfig;
use crate::services::{AsyncService, TaskJournal};

const SEARCH_INDEX_TASK_BUFFER: usize = 100;

pub struct SearchIndexService {
    task_sender: mpsc::Sender<SearchIndexTask>,
    task_receiver: Option<mpsc::Receiver<SearchIndexTask>>,
    config: SearchIndexConfig,
    journal: TaskJournal<SearchIndexTask>,
}

impl SearchIndexService {
    pub fn new(config: SearchIndexConfig, pool: sqlx::SqlitePool) -> Self {
        let (task_sender, task_receiver) = mpsc::channel(SEARCH_INDEX_TASK_BUFFER);
        Self {
            task_sender,
            task_receiver: Some(task_receiver),
            config,
            journal: TaskJournal::new(pool, "search_index"),
        }
    }

    /// Handle a GenerateIndexes task: run generators, build a SQLite
    /// database, and upload the single `search.db` file.
    async fn handle_generate(&self, channel_id: &str, channel_name: &str, sha: &str) -> Result<()> {
        let start = Instant::now();
        info!(
            event = "search_index_generation_start",
            channel_id = %channel_id,
            channel_name = %channel_name,
            sha = %sha,
            "starting search index generation"
        );

        let flake_ref = format!("github:{}/{}/{}", "nixpkgs", "nixpkgs", sha);
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

        // Generate file entries (walks store paths).
        let store_outputs: HashMap<String, Vec<(String, String)>> = HashMap::new();
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
            channel_name: channel_name.to_string(),
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
        if let Err(e) = upload::upload_database(&self.config, channel_name, &db_data).await {
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
            channel_name = %channel_name,
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
                self.handle_generate(&channel_id, &channel.name, &sha).await
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
