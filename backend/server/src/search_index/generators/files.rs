// File index generator.
//
// Walks the store paths of successfully-built packages and collects
// all installed files. The result is a flat array of FileEntry structs
// ready for insertion into SQLite.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use anyhow::Result;
use tracing::{debug, info, warn};

use super::super::types::FileEntry;

/// Maximum number of file entries to collect (safety cap).
const MAX_FILE_ENTRIES: usize = 50_000_000;

/// Generate file entries from store outputs.
///
/// `store_outputs` maps attr to a list of `(output_name, store_path)`.
/// Only store paths that exist locally are walked. All files and
/// symlinks under each store path are collected recursively.
pub async fn generate_file_entries(
    store_outputs: &HashMap<String, Vec<(String, String)>>,
) -> Result<Vec<FileEntry>> {
    let start = Instant::now();
    let mut entries: Vec<FileEntry> = Vec::new();
    let mut truncated = false;

    for (attr, outputs) in store_outputs {
        if truncated {
            break;
        }
        for (output_name, store_path) in outputs {
            if entries.len() >= MAX_FILE_ENTRIES {
                warn!(
                    event = "files_index_truncated",
                    max = MAX_FILE_ENTRIES,
                    "file index reached entry cap; stopping collection"
                );
                truncated = true;
                break;
            }

            let path = Path::new(store_path);
            if !path.exists() {
                debug!(
                    event = "files_index_skip_missing",
                    store_path = %store_path,
                    attr = %attr,
                    "store path not present locally; skipping"
                );
                continue;
            }

            walk_store_path(path, path, attr, output_name, &mut entries).await;
        }
    }

    entries.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.package.cmp(&b.package)));

    info!(
        event = "file_entries_generated",
        entries = entries.len(),
        elapsed_ms = start.elapsed().as_millis() as u64,
        "generated file entries"
    );

    Ok(entries)
}

/// Recursively walk a store path and collect all files and symlinks.
///
/// `root` is the store path root used to compute relative paths.
/// `current` is the directory currently being walked.
async fn walk_store_path(
    root: &Path,
    current: &Path,
    attr: &str,
    output_name: &str,
    entries: &mut Vec<FileEntry>,
) {
    let mut read_dir = match tokio::fs::read_dir(current).await {
        Ok(rd) => rd,
        Err(e) => {
            debug!(
                event = "files_index_readdir_failed",
                path = %current.display(),
                error = %e,
                "failed to read directory; skipping"
            );
            return;
        },
    };

    while let Ok(Some(entry)) = read_dir.next_entry().await {
        if entries.len() >= MAX_FILE_ENTRIES {
            return;
        }

        let file_type = match entry.file_type().await {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        let entry_path = entry.path();

        if file_type.is_dir() {
            Box::pin(walk_store_path(
                root,
                &entry_path,
                attr,
                output_name,
                entries,
            ))
            .await;
            continue;
        }

        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }

        let relative = match entry_path.strip_prefix(root) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let rel_str = match relative.to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };

        entries.push(FileEntry {
            file: rel_str,
            package: attr.to_string(),
            output: output_name.to_string(),
        });
    }
}
