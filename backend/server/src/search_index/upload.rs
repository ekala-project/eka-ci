// Upload logic for search index files.
//
// Supports two backends:
//   - Local filesystem: writes files directly to a directory path.
//   - S3: shells out to `aws s3 cp` with credentials injected via env vars.
//
// All uploads are namespaced by channel name so multiple channels can
// coexist under the same destination without clobbering each other:
//   {destination}/{channel_name}/search.db

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use tracing::{debug, info};

use crate::config::{CredentialSource, SearchIndexConfig};

/// Compress the SQLite database with zstd and upload it to the
/// configured destination as `search.db.zst`, namespaced under the
/// channel name.
pub async fn upload_database(
    config: &SearchIndexConfig,
    channel_name: &str,
    data: &[u8],
) -> Result<()> {
    let compressed = compress_zstd(data)?;

    info!(
        event = "search_db_compressed",
        raw_bytes = data.len(),
        compressed_bytes = compressed.len(),
        "compressed search.db with zstd"
    );

    let dest = &config.destination;
    let relative = format!("{channel_name}/search.db.zst");

    if dest.starts_with("s3://") {
        upload_s3(
            dest,
            &relative,
            &compressed,
            "application/zstd",
            &config.credentials,
        )
        .await
    } else {
        upload_local(dest, &relative, &compressed).await
    }
}

/// Compress data with zstd at level 3.
fn compress_zstd(data: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = zstd::Encoder::new(Vec::new(), 3)?;
    encoder.write_all(data)?;
    let compressed = encoder.finish()?;
    Ok(compressed)
}

/// Write a file to a local directory, creating parent directories as needed.
async fn upload_local(dest_dir: &str, relative_path: &str, data: &[u8]) -> Result<()> {
    let path = Path::new(dest_dir).join(relative_path);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create directory: {}", parent.display()))?;
    }

    tokio::fs::write(&path, data)
        .await
        .with_context(|| format!("failed to write index file: {}", path.display()))?;

    debug!(
        event = "index_uploaded_local",
        path = %path.display(),
        bytes = data.len(),
        "wrote index file to local path"
    );
    Ok(())
}

/// Upload a file to S3 via `aws s3 cp`.
///
/// Uses a temporary file to avoid piping large blobs through stdin.
async fn upload_s3(
    s3_prefix: &str,
    relative_path: &str,
    data: &[u8],
    content_type: &str,
    credentials: &CredentialSource,
) -> Result<()> {
    let s3_url = format!("{}/{}", s3_prefix.trim_end_matches('/'), relative_path);
    let cred_env = credentials
        .load()
        .await
        .with_context(|| format!("failed to load credentials for S3 upload to {s3_url}"))?;

    let temp_file = write_temp_file(data).await?;
    let temp_path = temp_file.path().to_string_lossy().to_string();

    let output = build_s3_command(&temp_path, &s3_url, content_type, &cred_env)
        .output()
        .await
        .context("failed to spawn `aws s3 cp`")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "`aws s3 cp` to {s3_url} failed with {}: {}",
            output.status,
            stderr.chars().take(500).collect::<String>()
        );
    }

    info!(
        event = "index_uploaded_s3",
        url = %s3_url,
        bytes = data.len(),
        "uploaded index file to S3"
    );
    Ok(())
}

/// Write data to a temporary file and return the handle.
async fn write_temp_file(data: &[u8]) -> Result<tempfile::NamedTempFile> {
    let temp =
        tempfile::NamedTempFile::new().context("failed to create temp file for S3 upload")?;
    tokio::fs::write(temp.path(), data)
        .await
        .context("failed to write temp file for S3 upload")?;
    Ok(temp)
}

/// Build the `aws s3 cp` command with credentials injected.
fn build_s3_command(
    source: &str,
    dest: &str,
    content_type: &str,
    cred_env: &HashMap<String, String>,
) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("aws");
    cmd.args(["s3", "cp", source, dest, "--content-type", content_type]);
    for (key, value) in cred_env {
        cmd.env(key, value);
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);
    cmd
}
