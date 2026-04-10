use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::types::{HookContext, HookResult, HookTask, PostBuildHook};

pub struct HookExecutor {
    hook_receiver: mpsc::Receiver<HookTask>,
    logs_dir: PathBuf,
}

impl HookExecutor {
    pub fn new(hook_receiver: mpsc::Receiver<HookTask>, logs_dir: PathBuf) -> Self {
        Self {
            hook_receiver,
            logs_dir,
        }
    }

    pub async fn run(mut self, cancellation_token: CancellationToken) {
        info!("HookExecutor service starting");

        while let Some(request) = cancellation_token
            .run_until_cancelled(self.hook_receiver.recv())
            .await
        {
            let task = match request {
                Some(task) => task,
                None => {
                    warn!("Hook receiver channel closed, shutting down");
                    break;
                },
            };

            if let Err(e) = self.handle_hook_task(task).await {
                error!(error = %e, "Failed to handle hook task");
            }
        }

        info!("HookExecutor service shutdown gracefully");
    }

    async fn handle_hook_task(&mut self, task: HookTask) -> Result<()> {
        info!(
            "Executing {} hooks for drv: {} (job: {})",
            task.hooks.len(),
            task.drv_path,
            task.context.job_name
        );

        for hook in &task.hooks {
            match self.execute_hook(hook, &task).await {
                Ok(result) => {
                    if result.success {
                        info!(
                            "Hook '{}' succeeded for drv {} (exit_code: {})",
                            result.hook_name,
                            task.drv_path,
                            result.exit_code.unwrap_or(0)
                        );
                    } else {
                        warn!(
                            "Hook '{}' failed for drv {} (exit_code: {})",
                            result.hook_name,
                            task.drv_path,
                            result.exit_code.unwrap_or(-1)
                        );
                    }
                },
                Err(e) => {
                    error!(
                        "Failed to execute hook '{}' for drv {}: {}",
                        hook.name, task.drv_path, e
                    );
                },
            }
        }

        Ok(())
    }

    async fn execute_hook(&self, hook: &PostBuildHook, task: &HookTask) -> Result<HookResult> {
        debug!("Executing hook '{}': {:?}", hook.name, hook.command);

        // Create log directory for this drv if it doesn't exist
        let drv_hash = extract_drv_hash(&task.drv_path);
        let log_dir = self.logs_dir.join(&drv_hash);
        fs::create_dir_all(&log_dir)
            .await
            .context("failed to create log directory")?;

        let log_path = log_dir.join(format!("hook-{}.log", hook.name));

        // Build environment variables
        let env_vars = build_hook_env(&task.context, &task.drv_path, &task.out_paths, hook);

        // Execute the hook command
        let start = Instant::now();
        let (exit_code, success) = self
            .run_hook_command(&hook.command, &env_vars, &log_path)
            .await?;
        let duration = start.elapsed();

        debug!(
            "Hook '{}' completed in {:?} with exit code {:?}",
            hook.name, duration, exit_code
        );

        Ok(HookResult {
            hook_name: hook.name.clone(),
            exit_code,
            success,
            log_path: log_path.to_string_lossy().to_string(),
        })
    }

    async fn run_hook_command(
        &self,
        command: &[String],
        env_vars: &HashMap<String, String>,
        log_path: &PathBuf,
    ) -> Result<(Option<i32>, bool)> {
        if command.is_empty() {
            anyhow::bail!("Hook command is empty");
        }

        // Create a shell script that executes the command
        // This allows environment variable substitution
        let mut script = String::new();

        // Export all environment variables
        for (key, value) in env_vars {
            // Escape single quotes in values
            let escaped_value = value.replace("'", "'\\''");
            script.push_str(&format!("export {}='{}'\n", key, escaped_value));
        }

        // Add the command
        script.push_str(&shell_escape_command(command));

        debug!("Hook script:\n{}", script);

        // Execute with sh -c
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg(&script)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let output = cmd
            .output()
            .await
            .context("failed to execute hook command")?;

        // Write output to log file
        let mut log_file = fs::File::create(log_path)
            .await
            .context("failed to create log file")?;

        log_file
            .write_all(b"=== STDOUT ===\n")
            .await
            .context("failed to write to log file")?;
        log_file
            .write_all(&output.stdout)
            .await
            .context("failed to write stdout to log file")?;
        log_file
            .write_all(b"\n=== STDERR ===\n")
            .await
            .context("failed to write stderr separator")?;
        log_file
            .write_all(&output.stderr)
            .await
            .context("failed to write stderr to log file")?;
        log_file.flush().await.context("failed to flush log file")?;

        let exit_code = output.status.code();
        let success = output.status.success();

        Ok((exit_code, success))
    }
}

/// Extract the hash portion from a drv path
/// e.g., "/nix/store/abc123-foo.drv" -> "abc123-foo"
fn extract_drv_hash(drv_path: &str) -> String {
    drv_path
        .rsplit('/')
        .next()
        .unwrap_or(drv_path)
        .trim_end_matches(".drv")
        .to_string()
}

/// Build environment variables for the hook
fn build_hook_env(
    context: &HookContext,
    drv_path: &str,
    out_paths: &[String],
    hook: &PostBuildHook,
) -> HashMap<String, String> {
    let mut env = HashMap::new();

    // Nix-compatible variables
    env.insert("DRV_PATH".to_string(), drv_path.to_string());
    env.insert("OUT_PATHS".to_string(), out_paths.join(" "));

    // Extended eka-ci variables
    env.insert("EKA_JOB_NAME".to_string(), context.job_name.clone());
    env.insert(
        "EKA_IS_FOD".to_string(),
        if context.is_fod { "true" } else { "false" }.to_string(),
    );
    env.insert("EKA_SYSTEM".to_string(), context.system.clone());
    if let Some(pname) = &context.pname {
        env.insert("EKA_PNAME".to_string(), pname.clone());
    }
    env.insert(
        "EKA_BUILD_LOG_PATH".to_string(),
        context.build_log_path.clone(),
    );
    env.insert("EKA_COMMIT_SHA".to_string(), context.commit_sha.clone());

    // Custom environment variables from hook config
    for (key, value) in &hook.env {
        env.insert(key.clone(), value.clone());
    }

    env
}

/// Escape a command array for shell execution
fn shell_escape_command(command: &[String]) -> String {
    command
        .iter()
        .map(|arg| {
            // If the argument contains special characters or spaces, quote it
            if arg.contains(' ')
                || arg.contains('$')
                || arg.contains('*')
                || arg.contains('?')
                || arg.contains('[')
                || arg.contains(']')
            {
                format!("'{}'", arg.replace("'", "'\\''"))
            } else {
                arg.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_drv_hash() {
        assert_eq!(extract_drv_hash("/nix/store/abc123-foo.drv"), "abc123-foo");
        assert_eq!(
            extract_drv_hash("/nix/store/xyz789-bar-1.0.drv"),
            "xyz789-bar-1.0"
        );
        assert_eq!(extract_drv_hash("simple.drv"), "simple");
    }

    #[test]
    fn test_build_hook_env() {
        let context = HookContext {
            job_name: "my-job".to_string(),
            is_fod: true,
            system: "x86_64-linux".to_string(),
            pname: Some("my-package".to_string()),
            build_log_path: "/logs/abc/build.log".to_string(),
            commit_sha: "abc123def456".to_string(),
        };

        let mut custom_env = HashMap::new();
        custom_env.insert("CUSTOM_VAR".to_string(), "custom_value".to_string());

        let hook = PostBuildHook {
            name: "test-hook".to_string(),
            command: vec!["echo".to_string(), "test".to_string()],
            env: custom_env,
        };

        let drv_path = "/nix/store/abc123-foo.drv";
        let out_paths = vec![
            "/nix/store/out1-foo".to_string(),
            "/nix/store/out2-foo-dev".to_string(),
        ];

        let env = build_hook_env(&context, drv_path, &out_paths, &hook);

        assert_eq!(env.get("DRV_PATH").unwrap(), drv_path);
        assert_eq!(
            env.get("OUT_PATHS").unwrap(),
            "/nix/store/out1-foo /nix/store/out2-foo-dev"
        );
        assert_eq!(env.get("EKA_JOB_NAME").unwrap(), "my-job");
        assert_eq!(env.get("EKA_IS_FOD").unwrap(), "true");
        assert_eq!(env.get("EKA_SYSTEM").unwrap(), "x86_64-linux");
        assert_eq!(env.get("EKA_PNAME").unwrap(), "my-package");
        assert_eq!(
            env.get("EKA_BUILD_LOG_PATH").unwrap(),
            "/logs/abc/build.log"
        );
        assert_eq!(env.get("EKA_COMMIT_SHA").unwrap(), "abc123def456");
        assert_eq!(env.get("CUSTOM_VAR").unwrap(), "custom_value");
    }

    #[test]
    fn test_shell_escape_command() {
        assert_eq!(
            shell_escape_command(&["echo".to_string(), "hello".to_string()]),
            "echo hello"
        );
        assert_eq!(
            shell_escape_command(&["echo".to_string(), "hello world".to_string()]),
            "echo 'hello world'"
        );
        assert_eq!(
            shell_escape_command(&[
                "nix".to_string(),
                "copy".to_string(),
                "--to".to_string(),
                "s3://cache".to_string()
            ]),
            "nix copy --to s3://cache"
        );
        assert_eq!(
            shell_escape_command(&["echo".to_string(), "$OUT_PATHS".to_string()]),
            "echo '$OUT_PATHS'"
        );
    }
}
