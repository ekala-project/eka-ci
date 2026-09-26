// Upload logic for search index files.
//
// Supports two backends:
//   - Local filesystem: writes files directly to a directory path.
//   - S3: shells out to `aws s3 cp` with credentials injected via env vars.
//
// The S3 path reuses the CredentialSource infrastructure from the cache
// registry so all existing auth methods (env, file, AWS profile, Vault,
// Secrets Manager, IMDS, systemd-creds) work out of the box.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use tracing::{debug, info};

use crate::config::{CredentialSource, SearchIndexConfig};
use crate::search_index::types::Manifest;

/// Upload a single compressed index file to the configured destination.
pub async fn upload_index(config: &SearchIndexConfig, filename: &str, data: &[u8]) -> Result<()> {
    let dest = &config.destination;

    if dest.starts_with("s3://") {
        upload_s3(dest, filename, data, &config.credentials).await
    } else {
        upload_local(dest, filename, data).await
    }
}

/// Upload the manifest.json (uncompressed) to the configured destination.
pub async fn upload_manifest(config: &SearchIndexConfig, manifest: &Manifest) -> Result<()> {
    let json = serde_json::to_vec_pretty(manifest).context("failed to serialize manifest")?;
    let dest = &config.destination;

    if dest.starts_with("s3://") {
        upload_s3(dest, "manifest.json", &json, &config.credentials).await
    } else {
        upload_local(dest, "manifest.json", &json).await
    }
}

/// Write a file to a local directory.
async fn upload_local(dest_dir: &str, filename: &str, data: &[u8]) -> Result<()> {
    let dir = Path::new(dest_dir);
    tokio::fs::create_dir_all(dir)
        .await
        .with_context(|| format!("failed to create index directory: {}", dir.display()))?;

    let path = dir.join(filename);
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
    filename: &str,
    data: &[u8],
    credentials: &CredentialSource,
) -> Result<()> {
    let s3_url = format!("{}/{}", s3_prefix.trim_end_matches('/'), filename);
    let cred_env = credentials
        .load()
        .await
        .with_context(|| format!("failed to load credentials for S3 upload to {s3_url}"))?;

    let temp_file = write_temp_file(data).await?;
    let temp_path = temp_file.path().to_string_lossy().to_string();

    let content_type = if filename.ends_with(".zst") {
        "application/zstd"
    } else {
        "application/json"
    };

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
