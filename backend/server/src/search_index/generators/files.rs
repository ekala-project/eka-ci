// File index generator.
//
// Walks the store paths of successfully-built packages and collects
// executables under bin/, sbin/, and libexec/. The result is a flat
// array of FileEntry structs, compressed with zstd.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use super::super::types::FileEntry;

/// Maximum number of file entries to collect (safety cap).
const MAX_FILE_ENTRIES: usize = 5_000_000;

/// Generate the compressed `files.json.zst` index.
///
/// `store_outputs` maps `(attr, output_name)` to the store path.
/// Only store paths that exist locally are walked.
pub async fn generate_files_index(
    store_outputs: &HashMap<String, Vec<(String, String)>>,
) -> Result<(Vec<u8>, usize)> {
    let start = Instant::now();
    let mut entries: Vec<FileEntry> = Vec::new();

    for (attr, outputs) in store_outputs {
        for (output_name, store_path) in outputs {
            if entries.len() >= MAX_FILE_ENTRIES {
                warn!(
                    event = "files_index_truncated",
                    max = MAX_FILE_ENTRIES,
                    "file index reached entry cap; stopping collection"
                );
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

            collect_executables(path, attr, output_name, &mut entries).await;
        }
    }

    entries.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.package.cmp(&b.package)));

    let count = entries.len();
    let compressed = compress_json(&entries)?;

    info!(
        event = "files_index_generated",
        entries = count,
        compressed_bytes = compressed.len(),
        elapsed_ms = start.elapsed().as_millis() as u64,
        "generated files.json.zst"
    );

    Ok((compressed, count))
}

/// Walk `{store_path}/{bin,sbin,libexec}` and collect executable files
/// and symlinks.
async fn collect_executables(
    store_path: &Path,
    attr: &str,
    output_name: &str,
    entries: &mut Vec<FileEntry>,
) {
    let dirs = ["bin", "sbin", "libexec"];
    for dir in dirs {
        let dir_path = store_path.join(dir);
        if !dir_path.is_dir() {
            continue;
        }
        let read_dir = match tokio::fs::read_dir(&dir_path).await {
            Ok(rd) => rd,
            Err(e) => {
                debug!(
                    event = "files_index_readdir_failed",
                    path = %dir_path.display(),
                    error = %e,
                    "failed to read directory; skipping"
                );
                continue;
            },
        };
        collect_from_dir(read_dir, dir, attr, output_name, entries).await;
    }
}

/// Read directory entries and add files/symlinks to the entries list.
async fn collect_from_dir(
    mut read_dir: tokio::fs::ReadDir,
    prefix_dir: &str,
    attr: &str,
    output_name: &str,
    entries: &mut Vec<FileEntry>,
) {
    while let Ok(Some(entry)) = read_dir.next_entry().await {
        let file_type = match entry.file_type().await {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if !file_type.is_file() && !file_type.is_symlink() {
            continue;
        }
        let file_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };
        entries.push(FileEntry {
            file: format!("{prefix_dir}/{file_name}"),
            package: attr.to_string(),
            output: output_name.to_string(),
        });
        if entries.len() >= MAX_FILE_ENTRIES {
            return;
        }
    }
}

/// Serialize to JSON and compress with zstd (level 3).
fn compress_json<T: serde::Serialize>(data: &T) -> Result<Vec<u8>> {
    let json = serde_json::to_vec(data).context("failed to serialize files index JSON")?;
    let mut encoder = zstd::Encoder::new(Vec::new(), 3)?;
    encoder.write_all(&json)?;
    let compressed = encoder.finish()?;
    Ok(compressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compress_roundtrips() {
        let data = vec![FileEntry {
            file: "bin/hello".to_string(),
            package: "hello".to_string(),
            output: "out".to_string(),
        }];
        let compressed = compress_json(&data).unwrap();
        let decompressed = zstd::decode_all(compressed.as_slice()).unwrap();
        let roundtrip: Vec<FileEntry> = serde_json::from_slice(&decompressed).unwrap();
        assert_eq!(roundtrip.len(), 1);
        assert_eq!(roundtrip[0].file, "bin/hello");
    }
}
