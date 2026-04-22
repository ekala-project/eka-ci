use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::types::{HookContext, HookResult, HookTask, PostBuildHook};

/// Maximum expanded command arg length. Guards against pathological
/// substitutions (e.g. an attacker-controlled env value exploding a
/// single arg into gigabytes via repeated `${VAR}` references).
const MAX_EXPANDED_ARG_LEN: usize = 1 << 20; // 1 MiB

/// Maximum substitution recursion depth. Substitution is single-pass
/// (we do not re-expand output), so this is effectively the number of
/// `${...}` references permitted in one argument.
const MAX_SUBSTITUTIONS_PER_ARG: usize = 1024;

pub struct HookExecutor {
    hook_receiver: mpsc::Receiver<HookTask>,
    logs_dir: PathBuf,
    max_hook_timeout: Duration,
    audit_enabled: bool,
}

impl HookExecutor {
    pub fn new(
        hook_receiver: mpsc::Receiver<HookTask>,
        logs_dir: PathBuf,
        max_hook_timeout_seconds: u64,
        audit_enabled: bool,
    ) -> Self {
        Self {
            hook_receiver,
            logs_dir,
            max_hook_timeout: Duration::from_secs(max_hook_timeout_seconds),
            audit_enabled,
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

                    // Send result back through channel if provided
                    if let Some(ref sender) = task.result_sender {
                        if let Err(e) = sender.send(result).await {
                            error!("Failed to send hook result back through channel: {}", e);
                        }
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

        // Audit log: Hook execution start
        if self.audit_enabled {
            info!(
                event = "hook_execution_start",
                hook_name = %hook.name,
                drv_path = %task.drv_path,
                job_name = %task.context.job_name,
                commit_sha = %task.context.commit_sha,
                "Starting hook execution"
            );
        }

        // Create log directory for this drv if it doesn't exist
        let drv_hash = extract_drv_hash(&task.drv_path);
        let log_dir = self.logs_dir.join(&drv_hash);
        fs::create_dir_all(&log_dir)
            .await
            .context("failed to create log directory")?;

        let log_path = log_dir.join(format!("hook-{}.log", hook.name));

        // Build environment variables
        let env_vars = build_hook_env(&task.context, &task.drv_path, &task.out_paths, hook);

        // Execute the hook command with timeout
        let started_at = chrono::Utc::now();
        let start = Instant::now();
        let result = self
            .run_hook_command(&hook.command, &env_vars, &log_path)
            .await;
        let duration = start.elapsed();
        let completed_at = chrono::Utc::now();

        let (exit_code, success) = match result {
            Ok(result) => result,
            Err(e) => {
                // Check if this was a timeout error
                let is_timeout = e.to_string().contains("timeout");

                // Audit log: Hook execution failed/timeout
                if self.audit_enabled {
                    if is_timeout {
                        warn!(
                            event = "hook_execution_timeout",
                            hook_name = %hook.name,
                            drv_path = %task.drv_path,
                            job_name = %task.context.job_name,
                            timeout_seconds = self.max_hook_timeout.as_secs(),
                            duration_seconds = duration.as_secs(),
                            "Hook execution timed out"
                        );
                    } else {
                        error!(
                            event = "hook_execution_error",
                            hook_name = %hook.name,
                            drv_path = %task.drv_path,
                            job_name = %task.context.job_name,
                            error = %e,
                            duration_seconds = duration.as_secs(),
                            "Hook execution failed with error"
                        );
                    }
                }
                return Err(e);
            },
        };

        debug!(
            "Hook '{}' completed in {:?} with exit code {:?}",
            hook.name, duration, exit_code
        );

        // Audit log: Hook execution completion
        if self.audit_enabled {
            info!(
                event = "hook_execution_complete",
                hook_name = %hook.name,
                drv_path = %task.drv_path,
                job_name = %task.context.job_name,
                commit_sha = %task.context.commit_sha,
                exit_code = ?exit_code,
                success = success,
                duration_seconds = duration.as_secs(),
                duration_ms = duration.as_millis(),
                "Hook execution completed"
            );
        }

        Ok(HookResult {
            hook_name: hook.name.clone(),
            exit_code,
            success,
            log_path: log_path.to_string_lossy().to_string(),
            drv_path: task.drv_path.clone(),
            started_at,
            completed_at,
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

        // Validate every env var key before we hand the map to the
        // child. Invalid keys (e.g. `IFS=;foo`) previously injected
        // shell code via the `export k='v'` emitter; we no longer
        // use a shell, but invalid keys are still rejected as a
        // defence in depth and so that downstream tooling sees a
        // clean environment.
        for key in env_vars.keys() {
            validate_env_key(key).with_context(|| format!("invalid hook env key: {key:?}"))?;
        }

        // Expand `$VAR` / `${VAR}` tokens in each argument against
        // the assembled env map. This replaces the shell's job
        // without ever going through a shell, so metacharacters in
        // arg values (backticks, pipes, redirections, semicolons,
        // newlines, etc.) remain literal bytes in that `argv[i]`.
        let expanded: Vec<String> = command
            .iter()
            .enumerate()
            .map(|(idx, arg)| {
                expand_vars(arg, env_vars)
                    .with_context(|| format!("failed to expand hook argv[{idx}]: {arg:?}"))
            })
            .collect::<Result<_>>()?;

        debug!(
            program = %expanded[0],
            argc = expanded.len(),
            "Executing hook"
        );

        // Build the child. We explicitly clear the inherited
        // environment and install only the validated, caller-
        // supplied map, so that secrets / CI-server env state do
        // not leak into hooks.
        let mut cmd = Command::new(&expanded[0]);
        cmd.args(&expanded[1..])
            .env_clear()
            .envs(env_vars.iter())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = cmd.spawn().context("failed to spawn hook command")?;

        // Run to completion with a wall-clock timeout. On timeout
        // the `wait_with_output` future is cancelled, which drops
        // the `Child` it owns; because we set `kill_on_drop(true)`
        // on the `Command` that drop issues SIGKILL and reaps the
        // child, preventing zombies (fixes M8).
        let output =
            match tokio::time::timeout(self.max_hook_timeout, child.wait_with_output()).await {
                Ok(result) => result.context("failed to execute hook command")?,
                Err(_) => {
                    return Err(anyhow!(
                        "Hook execution timed out after {} seconds",
                        self.max_hook_timeout.as_secs()
                    ));
                },
            };

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

/// Validate that `key` is a well-formed POSIX env-var name:
/// `[A-Za-z_][A-Za-z0-9_]*`. Rejecting anything else at execute
/// time prevents CR/LF / `=` / whitespace in keys, which were the
/// historical shell-injection vector when this crate still wrote
/// `export k='v'` lines into an `sh -c` script.
fn validate_env_key(key: &str) -> Result<()> {
    if key.is_empty() {
        anyhow::bail!("env var key is empty");
    }
    let mut bytes = key.bytes();
    let first = bytes.next().unwrap();
    let first_ok = first.is_ascii_alphabetic() || first == b'_';
    if !first_ok {
        anyhow::bail!("env var key must start with [A-Za-z_]: {key:?}");
    }
    for b in bytes {
        let ok = b.is_ascii_alphanumeric() || b == b'_';
        if !ok {
            anyhow::bail!("env var key has disallowed byte 0x{b:02x}: {key:?}");
        }
    }
    Ok(())
}

/// Expand `$NAME` / `${NAME}` tokens in `input` against `env`. This
/// is a deliberately small, shell-like substitution that only
/// understands variable references — no command substitution, no
/// globbing, no parameter-expansion modifiers. A literal `$` can be
/// produced by writing `$$`.
///
/// Rules:
/// * `$NAME` matches `[A-Za-z_][A-Za-z0-9_]*` greedily.
/// * `${NAME}` requires a closing `}` and validates `NAME` the same way; missing `}` or invalid
///   inner name is an error.
/// * `$$` → literal `$`.
/// * A `$` followed by any other character is an error (rather than silently left literal, to avoid
///   surprises if callers assume POSIX-like expansion).
/// * Unknown variable names are an error, not silent empty-string substitution. This means a hook
///   that references `${UNSET}` fails loudly instead of running with an unexpected empty arg
///   (which, for `nix copy --to <dest> $OUT_PATHS`, would cause `nix copy` to read from stdin — a
///   genuine footgun in the old shell-based implementation).
fn expand_vars(input: &str, env: &HashMap<String, String>) -> Result<String> {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    let mut subs = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if b != b'$' {
            // Fast path: copy a run of non-`$` bytes.
            let start = i;
            while i < bytes.len() && bytes[i] != b'$' {
                i += 1;
            }
            // Safe: we only advanced over ASCII non-`$` bytes; UTF-8
            // multibyte sequences have their leading bytes ≥ 0x80, so
            // slicing here is on a valid UTF-8 boundary.
            out.push_str(&input[start..i]);
            if out.len() > MAX_EXPANDED_ARG_LEN {
                anyhow::bail!("expanded hook arg exceeded {} bytes", MAX_EXPANDED_ARG_LEN);
            }
            continue;
        }

        // Saw `$`.
        let next = bytes.get(i + 1).copied();
        match next {
            None => anyhow::bail!("dangling `$` at end of input"),
            Some(b'$') => {
                out.push('$');
                i += 2;
            },
            Some(b'{') => {
                // Find matching `}`.
                let name_start = i + 2;
                let Some(close_rel) = bytes[name_start..].iter().position(|&c| c == b'}') else {
                    anyhow::bail!("unterminated `${{` in hook arg");
                };
                let name_end = name_start + close_rel;
                let name = &input[name_start..name_end];
                validate_env_key(name).context("invalid ${...} name")?;
                let value = env
                    .get(name)
                    .ok_or_else(|| anyhow!("hook references unknown env var `{name}`"))?;
                out.push_str(value);
                i = name_end + 1;
                subs += 1;
            },
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => {
                // `$NAME` form. Consume the longest valid identifier.
                let name_start = i + 1;
                let mut j = name_start;
                while j < bytes.len() {
                    let cj = bytes[j];
                    let ok = cj.is_ascii_alphanumeric() || cj == b'_';
                    if !ok {
                        break;
                    }
                    j += 1;
                }
                let name = &input[name_start..j];
                let value = env
                    .get(name)
                    .ok_or_else(|| anyhow!("hook references unknown env var `{name}`"))?;
                out.push_str(value);
                i = j;
                subs += 1;
            },
            Some(c) => {
                anyhow::bail!(
                    "unexpected byte 0x{c:02x} after `$` in hook arg; use `$$` for a literal `$`"
                );
            },
        }

        if subs > MAX_SUBSTITUTIONS_PER_ARG {
            anyhow::bail!(
                "too many `${{...}}` substitutions in hook arg (limit {})",
                MAX_SUBSTITUTIONS_PER_ARG
            );
        }
        if out.len() > MAX_EXPANDED_ARG_LEN {
            anyhow::bail!("expanded hook arg exceeded {} bytes", MAX_EXPANDED_ARG_LEN);
        }
    }

    Ok(out)
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

    fn env_with(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // --- validate_env_key ---

    #[test]
    fn env_key_accepts_standard_names() {
        for key in ["OUT_PATHS", "DRV_PATH", "EKA_JOB_NAME", "_foo", "A1"] {
            validate_env_key(key).unwrap_or_else(|e| panic!("should accept {key}: {e}"));
        }
    }

    #[test]
    fn env_key_rejects_empty() {
        assert!(validate_env_key("").is_err());
    }

    #[test]
    fn env_key_rejects_leading_digit() {
        assert!(validate_env_key("1FOO").is_err());
    }

    #[test]
    fn env_key_rejects_injection_attempts() {
        // Historical shell-injection vectors: keys that carried
        // shell metacharacters to break out of the `export k='v'`
        // line that the old `sh -c` script writer emitted.
        for bad in [
            "IFS=;rm",  // `=` + `;`
            "FOO\nBAR", // newline
            "FOO BAR",  // space
            "FOO'; #",  // quote + `;` + `#`
            "FOO=`id`", // backticks
            "FOO$(id)", // command substitution
            "FOO|cat",  // pipe
            "FOO\0BAR", // NUL
        ] {
            assert!(
                validate_env_key(bad).is_err(),
                "env_key should reject {bad:?}"
            );
        }
    }

    // --- expand_vars ---

    #[test]
    fn expand_noop_for_plain_text() {
        let env = env_with(&[]);
        assert_eq!(expand_vars("hello world", &env).unwrap(), "hello world");
        assert_eq!(expand_vars("", &env).unwrap(), "");
    }

    #[test]
    fn expand_dollar_name_and_braces() {
        let env = env_with(&[("OUT_PATHS", "/nix/store/a /nix/store/b")]);
        assert_eq!(
            expand_vars("$OUT_PATHS", &env).unwrap(),
            "/nix/store/a /nix/store/b"
        );
        assert_eq!(
            expand_vars("${OUT_PATHS}", &env).unwrap(),
            "/nix/store/a /nix/store/b"
        );
    }

    #[test]
    fn expand_greedy_identifier_stops_at_nonword() {
        let env = env_with(&[("OUT", "/nix/store/x")]);
        // `$OUT.txt` → value of `OUT` followed by literal `.txt`.
        assert_eq!(expand_vars("$OUT.txt", &env).unwrap(), "/nix/store/x.txt");
    }

    #[test]
    fn expand_double_dollar_is_literal() {
        let env = env_with(&[]);
        assert_eq!(expand_vars("price: $$5", &env).unwrap(), "price: $5");
    }

    #[test]
    fn expand_unknown_var_is_error() {
        let env = env_with(&[]);
        assert!(expand_vars("$MISSING", &env).is_err());
        assert!(expand_vars("${ALSO_MISSING}", &env).is_err());
    }

    #[test]
    fn expand_rejects_unterminated_brace() {
        let env = env_with(&[("X", "y")]);
        assert!(expand_vars("${X", &env).is_err());
    }

    #[test]
    fn expand_rejects_invalid_brace_name() {
        let env = env_with(&[]);
        assert!(expand_vars("${1FOO}", &env).is_err());
        assert!(expand_vars("${}", &env).is_err());
    }

    #[test]
    fn expand_rejects_dangling_dollar() {
        let env = env_with(&[]);
        assert!(expand_vars("trailing $", &env).is_err());
    }

    #[test]
    fn expand_rejects_dollar_followed_by_punctuation() {
        let env = env_with(&[]);
        assert!(expand_vars("$-foo", &env).is_err());
        assert!(expand_vars("$ ", &env).is_err());
    }

    #[test]
    fn expand_leaves_shell_metacharacters_literal_in_value() {
        // This is the key safety property: an env value containing
        // `;rm -rf /` survives expansion as data, because the
        // substituted arg is then passed to `Command::args` without
        // ever going through a shell.
        let env = env_with(&[("DRV_PATH", "/nix/store/a; rm -rf / #")]);
        assert_eq!(
            expand_vars("$DRV_PATH", &env).unwrap(),
            "/nix/store/a; rm -rf / #"
        );
    }

    #[test]
    fn expand_supports_utf8_literals_alongside_ascii_vars() {
        let env = env_with(&[("NAME", "world")]);
        assert_eq!(
            expand_vars("héllo, $NAME 🚀", &env).unwrap(),
            "héllo, world 🚀"
        );
    }

    #[test]
    fn expand_rejects_excessive_output_size() {
        // A realistic abuse: attacker-controlled env value that is
        // large and referenced enough times to exceed the byte
        // cap. Stay under MAX_SUBSTITUTIONS_PER_ARG so we exercise
        // the size cap, not the substitution-count cap.
        let big = "A".repeat(4096);
        let env = env_with(&[("BIG", &big)]);
        let mut tpl = String::new();
        for _ in 0..512 {
            tpl.push_str("${BIG}");
        }
        // Expands to 512 * 4096 = 2 MiB (>> 1 MiB cap).
        let err = expand_vars(&tpl, &env).unwrap_err();
        assert!(
            err.to_string().contains("exceeded"),
            "expected size limit error, got: {err}"
        );
    }

    #[test]
    fn expand_rejects_excessive_substitution_count() {
        let env = env_with(&[("X", "a")]);
        let mut tpl = String::new();
        for _ in 0..(MAX_SUBSTITUTIONS_PER_ARG + 10) {
            tpl.push_str("${X}");
        }
        let err = expand_vars(&tpl, &env).unwrap_err();
        assert!(
            err.to_string().contains("too many"),
            "expected substitution-count error, got: {err}"
        );
    }
}
