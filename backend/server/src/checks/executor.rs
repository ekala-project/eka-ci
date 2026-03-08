use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, Result};
use birdcage::process::Command;
use birdcage::{Birdcage, Exception, Sandbox};
use tracing::{debug, info};

use super::{CheckResult, parse_env_output};
use crate::ci::config::Check;
use crate::github::CICheckInfo;

/// Execute a check in a sandboxed environment
///
/// This function:
/// 1. Creates a temporary checkout directory
/// 2. Gets the nix shell environment (from flake or shell.nix)
/// 3. Creates a birdcage sandbox with appropriate mounts
/// 4. Executes the command in the sandboxed checkout
pub async fn execute_check(
    check: &Check,
    repo_path: &Path,
    check_name: &str,
    ci_info: &CICheckInfo,
) -> Result<CheckResult> {
    info!("Executing check: {}", check_name);

    // Step 1: Create temporary directory for checkout
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;
    let checkout_path = temp_dir.path();

    // Copy repository to temp directory
    copy_repository(repo_path, checkout_path).await?;

    // Step 2: Get nix shell environment
    debug!(
        "Fetching nix shell environment (shell: {:?}, shell_nix: {})",
        check.shell, check.shell_nix
    );
    let env_vars = get_nix_shell_env(
        check.shell.as_deref(),
        check.shell_nix,
        checkout_path,
        ci_info,
    )
    .await?;

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
    debug!(
        "Copying repository from {} to {}",
        src.display(),
        dst.display()
    );

    // Use tokio to spawn blocking copy operation
    let src = src.to_path_buf();
    let dst = dst.to_path_buf();

    tokio::task::spawn_blocking(move || copy_dir_all(&src, &dst))
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

/// Get environment variables from nix shell (either flake or shell.nix)
///
/// For base branch commits, this function creates a gcroot to preserve the dev shell
/// and avoid rebuilding it between CI runs.
async fn get_nix_shell_env(
    shell: Option<&str>,
    shell_nix: bool,
    checkout_path: &Path,
    ci_info: &CICheckInfo,
) -> Result<std::collections::HashMap<String, String>> {
    use std::process::Stdio;

    if shell_nix {
        // shell.nix mode
        let shell_nix_path = checkout_path.join("shell.nix");
        if !shell_nix_path.exists() {
            anyhow::bail!(
                "No shell.nix found in repository. When shell_nix is true, a shell.nix file is \
                 required. Please add shell.nix or set shell_nix to false."
            );
        }

        // Determine if this is a base branch commit (no base_commit means this IS the base)
        let is_base_branch = ci_info.base_commit.is_none();

        let mut cmd = tokio::process::Command::new("nix-shell");
        cmd.env_clear().current_dir(checkout_path).arg("shell.nix");

        if let Some(shell_name) = shell {
            cmd.arg("-A").arg(shell_name);
        }

        // For base branch, create a gcroot using --profile
        if is_base_branch {
            let cache_dir = super::get_cache_dir();
            let shell_name = shell.unwrap_or("default");
            let profile_path = cache_dir
                .join("gcroots")
                .join(&ci_info.owner)
                .join(&ci_info.repo_name)
                .join(shell_name)
                .join(&ci_info.commit);

            // Ensure parent directory exists
            if let Some(parent) = profile_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .context("Failed to create gcroot directory")?;
            }

            debug!(
                "Creating gcroot for base branch at {}",
                profile_path.display()
            );

            // Note: nix-shell doesn't support --profile, so we need to use a different approach
            // We'll use --indirect to create an indirect gcroot
            // Actually, nix-shell doesn't have great gcroot support, so we'll document this limitation
            // For now, users should prefer flake mode for base branch gcroot support
            debug!("Warning: gcroot creation for shell.nix mode is not fully supported yet");
        }

        cmd.arg("--run")
            .arg("env")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        debug!(
            "Running nix-shell shell.nix{} --run env {}",
            shell.map(|s| format!(" -A {}", s)).unwrap_or_default(),
            if is_base_branch {
                "(gcroot support limited for shell.nix)"
            } else {
                ""
            }
        );

        let output = cmd.output().await.context("Failed to execute nix-shell")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("nix-shell failed: {}", stderr);
        }

        let stdout = String::from_utf8(output.stdout)
            .context("Failed to parse nix-shell output as UTF-8")?;

        Ok(parse_env_output(&stdout))
    } else {
        // flake mode
        let flake_path = checkout_path.join("flake.nix");
        if !flake_path.exists() {
            anyhow::bail!(
                "No flake.nix found in repository. The checks feature requires a flake.nix file. \
                 Please add a flake.nix with devShells or set shell_nix to true to use shell.nix."
            );
        }

        // Build the flake reference
        let flake_ref = match shell {
            Some(shell_name) => format!(".#{}", shell_name),
            None => ".".to_string(),
        };

        // Determine if this is a base branch commit (no base_commit means this IS the base)
        let is_base_branch = ci_info.base_commit.is_none();

        // For base branch, create a gcroot to preserve the dev shell
        if is_base_branch {
            let cache_dir = super::get_cache_dir();
            let shell_name = shell.unwrap_or("default");
            let gcroot_path = cache_dir
                .join("gcroots")
                .join(&ci_info.owner)
                .join(&ci_info.repo_name)
                .join(shell_name)
                .join(&ci_info.commit);

            // Ensure parent directory exists
            if let Some(parent) = gcroot_path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .context("Failed to create gcroot directory")?;
            }

            debug!(
                "Creating gcroot for base branch at {}",
                gcroot_path.display()
            );

            // Step 1: Build the dev shell and get its store path
            let build_output = tokio::process::Command::new("nix")
                .env_clear()
                .current_dir(checkout_path)
                .arg("build")
                .arg(&flake_ref)
                .arg("--print-out-paths")
                .arg("--no-link") // Don't create a result symlink
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .context("Failed to execute nix build")?;

            if !build_output.status.success() {
                let stderr = String::from_utf8_lossy(&build_output.stderr);
                anyhow::bail!("nix build failed: {}", stderr);
            }

            let store_path = String::from_utf8(build_output.stdout)
                .context("Failed to parse nix build output as UTF-8")?
                .trim()
                .to_string();

            debug!("Dev shell built at: {}", store_path);

            // Step 2: Create indirect gcroot
            let gcroot_output = tokio::process::Command::new("nix-store")
                .arg("--realise")
                .arg(&store_path)
                .arg("--add-root")
                .arg(&gcroot_path)
                .arg("--indirect")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
                .context("Failed to create gcroot")?;

            if !gcroot_output.status.success() {
                let stderr = String::from_utf8_lossy(&gcroot_output.stderr);
                anyhow::bail!("nix-store --add-root failed: {}", stderr);
            }

            debug!("Created indirect gcroot at {}", gcroot_path.display());
        }

        // Get environment from dev shell
        debug!("Running nix develop {} --command env", flake_ref);

        let output = tokio::process::Command::new("nix")
            .env_clear()
            .current_dir(checkout_path)
            .arg("develop")
            .arg(&flake_ref)
            .arg("--command")
            .arg("env")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to execute nix develop")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("nix develop failed: {}", stderr);
        }

        let stdout = String::from_utf8(output.stdout)
            .context("Failed to parse nix develop output as UTF-8")?;

        Ok(parse_env_output(&stdout))
    }
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
    use std::fs;

    use super::*;

    #[tokio::test]
    #[ignore] // Requires nix develop to be available and a flake.nix
    async fn test_get_nix_shell_env_flake() {
        // This test requires a repository with a flake.nix
        // Create a temporary directory with a minimal flake
        let temp_dir = tempfile::tempdir().unwrap();
        let flake_content = r#"{
  description = "Test flake";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { nixpkgs, ... }: {
    devShells.x86_64-linux.default = nixpkgs.legacyPackages.x86_64-linux.mkShell {
      packages = [ nixpkgs.legacyPackages.x86_64-linux.hello ];
    };
  };
}"#;
        fs::write(temp_dir.path().join("flake.nix"), flake_content).unwrap();

        // Create a test CI info (head commit, so no gcroot)
        let ci_info = CICheckInfo {
            commit: "test123".to_string(),
            base_commit: Some("base456".to_string()),
            owner: "testowner".to_string(),
            repo_name: "testrepo".to_string(),
        };

        let env = get_nix_shell_env(None, false, temp_dir.path(), &ci_info).await;

        // Should succeed if nix is installed
        if let Ok(env_vars) = env {
            assert!(env_vars.contains_key("PATH"));
            // PATH should contain a /nix/store path
            let path = env_vars.get("PATH").unwrap();
            assert!(path.contains("/nix/store"));
        }
    }

    #[tokio::test]
    #[ignore] // Requires nix develop and creates temporary files
    async fn test_execute_check_basic() {
        // Create a temporary directory with a simple test file and flake
        let temp_dir = tempfile::tempdir().unwrap();
        let test_file = temp_dir.path().join("test.txt");
        fs::write(&test_file, "test content").unwrap();

        // Create a minimal flake.nix
        let flake_content = r#"{
  description = "Test flake";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { nixpkgs, ... }: {
    devShells.x86_64-linux.default = nixpkgs.legacyPackages.x86_64-linux.mkShell {
      packages = [ nixpkgs.legacyPackages.x86_64-linux.coreutils ];
    };
  };
}"#;
        fs::write(temp_dir.path().join("flake.nix"), flake_content).unwrap();

        // Create a check that reads the file
        let check = Check {
            shell: None,
            shell_nix: false,
            command: "cat test.txt".to_string(),
            allow_network: false,
        };

        // Create a test CI info (head commit, so no gcroot)
        let ci_info = CICheckInfo {
            commit: "test123".to_string(),
            base_commit: Some("base456".to_string()),
            owner: "testowner".to_string(),
            repo_name: "testrepo".to_string(),
        };

        // Execute the check
        let result = execute_check(&check, temp_dir.path(), "test-check", &ci_info).await;

        // Verify the result
        assert!(result.is_ok());
        let check_result = result.unwrap();
        assert!(check_result.success);
        assert_eq!(check_result.exit_code, 0);
        assert!(check_result.stdout.contains("test content"));
    }

    #[tokio::test]
    #[ignore] // Requires nix develop
    async fn test_execute_check_failure() {
        // Create a temporary directory
        let temp_dir = tempfile::tempdir().unwrap();

        // Create a minimal flake.nix
        let flake_content = r#"{
  description = "Test flake";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { nixpkgs, ... }: {
    devShells.x86_64-linux.default = nixpkgs.legacyPackages.x86_64-linux.mkShell {
      packages = [ nixpkgs.legacyPackages.x86_64-linux.coreutils ];
    };
  };
}"#;
        fs::write(temp_dir.path().join("flake.nix"), flake_content).unwrap();

        // Create a check that will fail
        let check = Check {
            shell: None,
            shell_nix: false,
            command: "cat nonexistent-file.txt".to_string(),
            allow_network: false,
        };

        // Create a test CI info (head commit, so no gcroot)
        let ci_info = CICheckInfo {
            commit: "test123".to_string(),
            base_commit: Some("base456".to_string()),
            owner: "testowner".to_string(),
            repo_name: "testrepo".to_string(),
        };

        // Execute the check
        let result = execute_check(&check, temp_dir.path(), "failing-check", &ci_info).await;

        // Verify the result indicates failure
        assert!(result.is_ok());
        let check_result = result.unwrap();
        assert!(!check_result.success);
        assert_ne!(check_result.exit_code, 0);
    }
}
