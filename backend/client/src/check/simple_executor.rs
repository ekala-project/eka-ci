use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::Command;
use tracing::debug;

/// Result of command execution
#[derive(Debug)]
pub struct ExecutionResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Execute command without sandboxing (for local development)
pub async fn execute_simple(
    repo_path: &Path,
    command: &str,
    env_vars: &HashMap<String, String>,
) -> Result<ExecutionResult> {
    debug!("Executing command in {}: {}", repo_path.display(), command);

    // Build the command with environment variables
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(repo_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Set environment variables
    for (key, value) in env_vars {
        cmd.env(key, value);
    }

    let output = cmd.output().await.context("Failed to execute command")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    Ok(ExecutionResult {
        exit_code,
        stdout,
        stderr,
    })
}
