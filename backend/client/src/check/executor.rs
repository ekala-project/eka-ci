use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::Result;
use ci_config::Check;
use tracing::{debug, info};

use super::nix_shell::get_nix_shell_env;
use super::simple_executor::{ExecutionResult, execute_simple};

/// Result of a check execution
#[derive(Debug)]
pub struct CheckResult {
    #[allow(dead_code)]
    pub check_name: String,
    pub success: bool,
    #[allow(dead_code)]
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration: Duration,
}

/// Check executor for running checks locally
pub struct CheckExecutor {
    repo_path: std::path::PathBuf,
}

impl CheckExecutor {
    pub fn new(repo_path: std::path::PathBuf) -> Self {
        Self { repo_path }
    }

    /// Execute a single check
    pub async fn execute_check(&self, check: &Check, check_name: &str) -> Result<CheckResult> {
        info!("Executing check: {}", check_name);

        let start = Instant::now();

        // Step 1: Get nix shell environment (if needed)
        // For commands like "nix build", we don't need a shell environment
        let needs_shell = !check.command.starts_with("nix build ");

        let env_vars = if needs_shell {
            debug!(
                "Fetching nix shell environment (shell: {:?}, shell_nix: {})",
                check.shell, check.shell_nix
            );
            get_nix_shell_env(check.shell.as_deref(), check.shell_nix, &self.repo_path).await?
        } else {
            debug!("Skipping shell environment for nix build command");
            HashMap::new()
        };

        // Step 2: Execute command (without sandboxing for local development)
        // Note: Full sandboxing is available when running checks through the CI server
        let result = self.execute_simple(&check.command, &env_vars).await?;

        let duration = start.elapsed();

        info!(
            "Check {} completed in {:?} with exit code {}",
            check_name, duration, result.exit_code
        );

        Ok(CheckResult {
            check_name: check_name.to_string(),
            success: result.exit_code == 0,
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
            duration,
        })
    }

    /// Execute command without sandboxing (for local development)
    async fn execute_simple(
        &self,
        command: &str,
        env_vars: &HashMap<String, String>,
    ) -> Result<ExecutionResult> {
        execute_simple(&self.repo_path, command, env_vars).await
    }
}
