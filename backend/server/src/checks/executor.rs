use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use birdcage::{Birdcage, Exception, Sandbox};
use birdcage::process::Command;
use tracing::{debug, info};

use crate::ci::config::Check;
use super::{CheckResult, parse_env_output};

/// Execute a check in a sandboxed environment
///
/// This function:
/// 1. Creates a temporary checkout directory
/// 2. Gets the nix-shell environment for the specified packages
/// 3. Creates a birdcage sandbox with appropriate mounts
/// 4. Executes the command in the sandboxed checkout
pub async fn execute_check(
    check: &Check,
    repo_path: &Path,
    check_name: &str,
) -> Result<CheckResult> {
    info!("Executing check: {}", check_name);

    // Step 1: Create temporary directory for checkout
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let checkout_path = temp_dir.path();

    // Copy repository to temp directory
    copy_repository(repo_path, checkout_path).await?;

    // Step 2: Get nix-shell environment
    debug!("Fetching nix-shell environment for packages: {:?}", check.packages);
    let env_vars = get_nix_shell_env(&check.packages).await?;

    // Step 3 & 4: Execute in sandbox
    let start = Instant::now();
    let result = execute_in_sandbox(
        checkout_path,
        &check.command,
        &env_vars,
        check.allow_network,
    )?;
    let duration_ms = start.elapsed().as_millis() as u64;

    info!(
        "Check {} completed in {}ms with exit code {}",
        check_name, duration_ms, result.exit_code
    );

    Ok(CheckResult::new(
        result.exit_code == 0,
        result.exit_code,
        result.stdout,
        result.stderr,
        duration_ms,
    ))
}

/// Copy repository contents to a temporary directory
async fn copy_repository(src: &Path, dst: &Path) -> Result<()> {
    debug!("Copying repository from {} to {}", src.display(), dst.display());

    // Use tokio to spawn blocking copy operation
    let src = src.to_path_buf();
    let dst = dst.to_path_buf();

    tokio::task::spawn_blocking(move || {
        copy_dir_all(&src, &dst)
    })
    .await
    .context("Failed to spawn copy task")?
    .context("Failed to copy repository")?;

    Ok(())
}

/// Recursively copy a directory
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if ty.is_dir() {
            // Skip common directories that don't need to be checked
            let dir_name = entry.file_name();
            if dir_name == ".git" || dir_name == "target" || dir_name == "node_modules" {
                continue;
            }
            copy_dir_all(&src_path, &dst_path)?;
        } else {
            std::fs::copy(src_path, dst_path)?;
        }
    }

    Ok(())
}

/// Get environment variables from nix-shell
async fn get_nix_shell_env(packages: &[String]) -> Result<std::collections::HashMap<String, String>> {
    use std::process::Stdio;

    let packages_arg = packages.join(" ");

    debug!("Running nix-shell -p {} --run 'env'", packages_arg);

    let output = tokio::process::Command::new("nix-shell")
        .env_clear() // Start with empty environment
        .arg("-p")
        .arg(&packages_arg)
        .arg("--run")
        .arg("env")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .context("Failed to execute nix-shell")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nix-shell failed: {}", stderr);
    }

    let stdout = String::from_utf8(output.stdout)
        .context("Failed to parse nix-shell output as UTF-8")?;

    Ok(parse_env_output(&stdout))
}

struct CommandResult {
    exit_code: i32,
    stdout: String,
    stderr: String,
}

/// Execute command in birdcage sandbox
fn execute_in_sandbox(
    checkout_path: &Path,
    command: &str,
    env_vars: &std::collections::HashMap<String, String>,
    allow_network: bool,
) -> Result<CommandResult> {
    use birdcage::process::Stdio;

    debug!("Setting up birdcage sandbox at {}", checkout_path.display());

    let mut sandbox = Birdcage::new();

    // Mount /nix/store as read-only
    sandbox
        .add_exception(Exception::Read(PathBuf::from("/nix/store")))
        .context("Failed to add /nix/store read exception")?;

    // Mount checkout directory as read-write
    sandbox
        .add_exception(Exception::WriteAndRead(checkout_path.to_path_buf()))
        .context("Failed to add checkout path write exception")?;

    // Mount .git directory as read-only (inside checkout)
    let git_dir = checkout_path.join(".git");
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
    // We need to export all environment variables and cd to the checkout path
    let mut script = String::new();
    for (key, value) in env_vars {
        // Escape single quotes in values
        let escaped_value = value.replace("'", "'\\''");
        script.push_str(&format!("export {}='{}'\n", key, escaped_value));
    }
    script.push_str(&format!("cd '{}'\n", checkout_path.display()));
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

    Ok(CommandResult {
        exit_code,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[tokio::test]
    #[ignore] // Requires nix-shell to be available
    async fn test_get_nix_shell_env() {
        // This test requires nix-shell to be available
        let packages = vec!["hello".to_string()];
        let env = get_nix_shell_env(&packages).await;

        // Should succeed if nix is installed
        if let Ok(env_vars) = env {
            assert!(env_vars.contains_key("PATH"));
            // PATH should contain a /nix/store path
            let path = env_vars.get("PATH").unwrap();
            assert!(path.contains("/nix/store"));
        }
    }

    #[tokio::test]
    #[ignore] // Requires nix-shell and creates temporary files
    async fn test_execute_check_basic() {
        // Create a temporary directory with a simple test file
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test content").unwrap();

        // Create a check that reads the file
        let check = Check {
            packages: vec!["coreutils".to_string()],
            command: "cat test.txt".to_string(),
            allow_network: false,
        };

        // Execute the check
        let result = execute_check(&check, temp_dir.path(), "test-check").await;

        // Verify the result
        assert!(result.is_ok());
        let check_result = result.unwrap();
        assert!(check_result.success);
        assert_eq!(check_result.exit_code, 0);
        assert!(check_result.stdout.contains("test content"));
    }

    #[tokio::test]
    #[ignore] // Requires nix-shell
    async fn test_execute_check_failure() {
        // Create a temporary directory
        let temp_dir = tempfile::tempdir().unwrap();

        // Create a check that will fail
        let check = Check {
            packages: vec!["coreutils".to_string()],
            command: "cat nonexistent-file.txt".to_string(),
            allow_network: false,
        };

        // Execute the check
        let result = execute_check(&check, temp_dir.path(), "failing-check").await;

        // Verify the result indicates failure
        assert!(result.is_ok());
        let check_result = result.unwrap();
        assert!(!check_result.success);
        assert_ne!(check_result.exit_code, 0);
    }
}
