use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use ci_config::Check;
use sandbox::DEFAULT_TIMEOUT;
use sandbox::checkout::Checkout;
use sandbox::devshell::DevShell;
use tracing::{info, warn};

use super::host::{self, HostOutput, Launcher};

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

#[derive(Debug, Clone)]
pub struct SandboxOptions {
    pub enabled: bool,
    pub network_allow: Vec<String>,
    pub timeout: Duration,
}

enum Backend {
    Host(Launcher),
    #[cfg(target_os = "linux")]
    Linux(sandbox::check::CheckSandbox),
}

/// Check executor for running checks locally
pub struct CheckExecutor {
    checkout: Checkout,
    backend: Backend,
    timeout: Duration,
}

impl CheckExecutor {
    pub fn new(repo_path: PathBuf, opts: &SandboxOptions) -> Result<Self> {
        let backend = if opts.enabled {
            sandboxed_backend(opts)?
        } else {
            warn!("running checks without a sandbox (--no-sandbox)");
            Backend::Host(Launcher::Direct)
        };
        Ok(Self {
            checkout: Checkout::resolve(repo_path),
            backend,
            timeout: opts.timeout,
        })
    }

    /// Execute a single check
    pub async fn execute_check(&self, check: &Check, check_name: &str) -> Result<CheckResult> {
        info!("Executing check: {}", check_name);
        let start = Instant::now();
        let shell = DevShell::for_check(&check.command, check.shell.as_deref(), check.shell_nix);
        let out = match &self.backend {
            Backend::Host(launcher) => self.run_on_host(launcher, check, shell).await?,
            #[cfg(target_os = "linux")]
            Backend::Linux(sb) => {
                let out = sb
                    .execute(&self.checkout, &check.command, shell, check.allow_network)
                    .await?;
                HostOutput {
                    exit_code: out.exit_code(),
                    timed_out: matches!(out.exit, sandbox::SandboxExit::TimedOut(_)),
                    stderr: out.stderr_with_timeout(),
                    stdout: out.stdout,
                }
            },
        };
        let duration = start.elapsed();
        info!(
            "Check {} completed in {:?} with exit code {}",
            check_name, duration, out.exit_code
        );
        Ok(CheckResult {
            check_name: check_name.to_string(),
            success: out.exit_code == 0,
            exit_code: out.exit_code,
            stdout: out.stdout,
            stderr: out.stderr,
            duration,
        })
    }

    async fn run_on_host(
        &self,
        launcher: &Launcher,
        check: &Check,
        shell: Option<DevShell<'_>>,
    ) -> Result<HostOutput> {
        let (cmd, net) = (&check.command, check.allow_network);
        #[cfg(target_os = "macos")]
        if (net || shell.is_some()) && matches!(launcher, Launcher::Seatbelt(_)) {
            warn!(
                "macOS sandbox: network access is not filtered (dev shell capture always has it, \
                 the check only with allow_network); private networks and cloud metadata \
                 endpoints are reachable, only localhost is blocked"
            );
        }
        let env = match shell {
            Some(shell) => {
                host::dev_shell_env(launcher, &self.checkout, shell, DEFAULT_TIMEOUT).await?
            },
            None => Vec::new(),
        };
        host::run(launcher, &self.checkout, cmd, net, &env, self.timeout).await
    }
}

#[cfg(target_os = "linux")]
fn sandboxed_backend(opts: &SandboxOptions) -> Result<Backend> {
    use std::sync::Arc;

    use anyhow::Context;
    use sandbox::check::{CheckSandbox, Limits};
    use sandbox::{NetworkPolicy, Sandbox};

    let allow = opts
        .network_allow
        .iter()
        .map(|c| c.parse().with_context(|| format!("--network-allow {c}")))
        .collect::<Result<_>>()?;
    let sandbox = Sandbox::locate(None)
        .context("sandbox unavailable; install bubblewrap and passt or use --no-sandbox")?
        .with_network_policy(NetworkPolicy {
            allow,
            deny: Vec::new(),
        });
    let check = Limits {
        timeout: opts.timeout,
        ..Limits::default()
    };
    Ok(Backend::Linux(CheckSandbox::new(Arc::new(sandbox), check)))
}

#[cfg(target_os = "macos")]
fn sandboxed_backend(opts: &SandboxOptions) -> Result<Backend> {
    if !opts.network_allow.is_empty() {
        warn!("--network-allow is ignored on macOS: Seatbelt cannot filter by address");
    }
    Ok(Backend::Host(Launcher::Seatbelt(
        super::seatbelt::Seatbelt::new()?,
    )))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn sandboxed_backend(_opts: &SandboxOptions) -> Result<Backend> {
    anyhow::bail!("no check sandbox is available on this platform; use --no-sandbox")
}
