// SearchIndexService: generates and uploads search indexes after
// channel promotion.
//
// This is a lightweight AsyncService that receives GenerateIndexes
// tasks from the ChannelService and runs the generators sequentially.
// Each generator is independent: if one fails, the others still run.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::types::{IndexInfo, Manifest, SearchIndexTask};
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

    /// Handle a GenerateIndexes task: run each generator, upload
    /// results, and produce a manifest.
    async fn handle_generate(&self, channel_id: &str, sha: &str) -> Result<()> {
        let start = Instant::now();
        info!(
            event = "search_index_generation_start",
            channel_id = %channel_id,
            sha = %sha,
            "starting search index generation"
        );

        // Determine the flake reference for nix search. Construct from
        // the channel's owner/repo and the promoted SHA.
        // For GitHub: "github:{owner}/{repo}/{sha}"
        let flake_ref = format!("github:{}/{}/{}", "nixpkgs", "nixpkgs", sha);
        let system = "x86_64-linux";

        let mut manifest_indexes: HashMap<String, IndexInfo> = HashMap::new();

        // Generate packages.json.zst
        match generators::packages::generate_packages_index(&flake_ref, system).await {
            Ok((data, count)) => {
                let size = data.len() as u64;
                if let Err(e) = upload::upload_index(&self.config, "packages.json.zst", &data).await
                {
                    warn!(
                        event = "search_index_upload_failed",
                        index = "packages",
                        error = %e,
                        "failed to upload packages index"
                    );
                } else {
                    manifest_indexes.insert(
                        "packages".to_string(),
                        IndexInfo {
                            size,
                            entries: count,
                        },
                    );
                }
            },
            Err(e) => {
                warn!(
                    event = "search_index_generation_failed",
                    index = "packages",
                    error = %e,
                    "failed to generate packages index"
                );
            },
        }

        // Generate files.json.zst (if enabled)
        if self.config.generate_files_index {
            // For now, pass an empty map — in a future enhancement the
            // service will query the DB for successfully-built drv output
            // paths from the promoted jobset.
            let store_outputs: HashMap<String, Vec<(String, String)>> = HashMap::new();
            match generators::files::generate_files_index(&store_outputs).await {
                Ok((data, count)) => {
                    let size = data.len() as u64;
                    if let Err(e) =
                        upload::upload_index(&self.config, "files.json.zst", &data).await
                    {
                        warn!(
                            event = "search_index_upload_failed",
                            index = "files",
                            error = %e,
                            "failed to upload files index"
                        );
                    } else {
                        manifest_indexes.insert(
                            "files".to_string(),
                            IndexInfo {
                                size,
                                entries: count,
                            },
                        );
                    }
                },
                Err(e) => {
                    warn!(
                        event = "search_index_generation_failed",
                        index = "files",
                        error = %e,
                        "failed to generate files index"
                    );
                },
            }
        }

        // Build and upload manifest
        let manifest = Manifest {
            generated_at: chrono::Utc::now().to_rfc3339(),
            nixpkgs_rev: sha.to_string(),
            indexes: manifest_indexes,
        };

        if let Err(e) = upload::upload_manifest(&self.config, &manifest).await {
            warn!(
                event = "search_index_manifest_upload_failed",
                error = %e,
                "failed to upload manifest.json"
            );
        }

        info!(
            event = "search_index_generation_complete",
            channel_id = %channel_id,
            sha = %sha,
            elapsed_ms = start.elapsed().as_millis() as u64,
            indexes = manifest.indexes.len(),
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
                channel: _,
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
                self.handle_generate(&channel_id, &sha).await
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
