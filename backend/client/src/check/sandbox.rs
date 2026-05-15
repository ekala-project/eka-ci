use anyhow::{Context, Result};
use birdcage::process::{Command, Stdio};
use birdcage::{Birdcage, Exception, Sandbox};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::debug;

/// Result of command execution
#[derive(Debug)]
pub struct ExecutionResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Execute command in birdcage sandbox
pub fn execute_in_sandbox(
    repo_path: &Path,
    command: &str,
    env_vars: &HashMap<String, String>,
    allow_network: bool,
) -> Result<ExecutionResult> {
    debug!("Setting up birdcage sandbox at {}", repo_path.display());

    let mut sandbox = Birdcage::new();

    // Mount /nix/store as read-only
    sandbox
        .add_exception(Exception::Read(PathBuf::from("/nix/store")))
        .context("Failed to add /nix/store read exception")?;

    // Mount repo directory as read-write
    sandbox
        .add_exception(Exception::WriteAndRead(repo_path.to_path_buf()))
        .context("Failed to add repo path write exception")?;

    // Mount .git directory as read-only (if it exists)
    let git_dir = repo_path.join(".git");
    if git_dir.exists() {
        sandbox
            .add_exception(Exception::Read(git_dir))
            .context("Failed to add .git read exception")?;
    }

    // Disable network if requested
    if !allow_network {
        sandbox
            .add_exception(Exception::Networking)
            .context("Failed to disable networking")?;
    }

    debug!("Executing command: {}", command);

    // Build the command with environment variables and directory change
    let mut script = String::new();
    for (key, value) in env_vars {
        // Escape single quotes in values
        let escaped_value = value.replace("'", "'\\''");
        script.push_str(&format!("export {}='{}'\n", key, escaped_value));
    }
    script.push_str(&format!("cd '{}'\n", repo_path.display()));
    script.push_str(command);

    // Execute command with sh -c using sandbox.spawn()
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&script)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let output = sandbox
        .spawn(cmd)
        .context("Failed to spawn command in sandbox")?
        .wait_with_output()
        .context("Failed to wait for command output")?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    Ok(ExecutionResult {
        exit_code,
        stdout,
        stderr,
    })
}
