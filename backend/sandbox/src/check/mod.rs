mod output;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
pub use output::OUTPUT_LIMIT;

use crate::checkout::Checkout;
use crate::devshell::{DevShell, parse_env};
use crate::{
    DEFAULT_MEMORY_LIMIT_MB, DEFAULT_TIMEOUT, Network, Sandbox, SandboxExit, SandboxSpec,
    resolve_program,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub timeout: Duration,
    pub memory_limit_mb: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            memory_limit_mb: DEFAULT_MEMORY_LIMIT_MB,
        }
    }
}

#[derive(Debug)]
pub struct CheckOutput {
    pub exit: SandboxExit,
    pub stdout: String,
    pub stderr: String,
}

impl CheckOutput {
    pub fn success(&self) -> bool {
        matches!(self.exit, SandboxExit::Exited(s) if s.success())
    }

    pub fn stderr_with_timeout(&self) -> String {
        match self.exit {
            SandboxExit::TimedOut(limit) => {
                format!("{}\n{}\n", self.stderr, crate::timeout_note(limit))
            },
            SandboxExit::Exited(_) => self.stderr.clone(),
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self.exit {
            SandboxExit::Exited(s) => s.code().unwrap_or(-1),
            SandboxExit::TimedOut(_) => -1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CheckSandbox {
    sandbox: Arc<Sandbox>,
    check: Limits,
    shell: Limits,
}

impl CheckSandbox {
    pub fn new(sandbox: Arc<Sandbox>, check: Limits) -> Self {
        Self {
            sandbox,
            check,
            shell: Limits::default(),
        }
    }

    pub fn with_dev_shell_limits(mut self, shell: Limits) -> Self {
        self.shell = shell;
        self
    }

    pub async fn execute(
        &self,
        checkout: &Checkout,
        command: &str,
        shell: Option<DevShell<'_>>,
        allow_network: bool,
    ) -> Result<CheckOutput> {
        let env = match shell {
            Some(shell) => self
                .dev_shell_env(checkout, shell)
                .await
                .context("failed to capture the dev shell environment")?,
            None => Vec::new(),
        };
        self.run(checkout, command, allow_network, &env)
            .await
            .context("failed to run check in sandbox")
    }

    pub async fn dev_shell_env(
        &self,
        checkout: &Checkout,
        shell: DevShell<'_>,
    ) -> Result<Vec<(String, String)>> {
        crate::devshell::require_entry_file(shell, checkout.root())?;
        let (name, args) = shell.command();
        let nix = resolve_program("nix")?;
        let program = nix.with_file_name(name);
        let spec = checkout_spec(program, checkout, self.shell)
            .args(args)
            .network(Network::Filtered);
        let out = self.spawn_and_capture(&spec, checkout).await?;
        match out.exit {
            SandboxExit::Exited(s) if s.success() => parse_env(out.stdout.as_bytes()),
            SandboxExit::TimedOut(t) => bail!("{name} timed out after {t:?}: {}", out.stderr),
            SandboxExit::Exited(s) => bail!("{name} failed ({s}): {}", out.stderr),
        }
    }

    pub async fn run(
        &self,
        checkout: &Checkout,
        command: &str,
        allow_network: bool,
        env: &[(String, String)],
    ) -> Result<CheckOutput> {
        let sh = shell_program()?;
        let nix = resolve_program("nix")?;
        let base_path = [sh.parent(), nix.parent()]
            .into_iter()
            .flatten()
            .map(|d| d.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(":");
        let network = if allow_network {
            Network::Filtered
        } else {
            Network::None
        };
        let mut spec = checkout_spec(sh, checkout, self.check)
            .args(["-c", command])
            .network(network);
        for (k, v) in env.iter().filter(|(k, _)| k != "PATH") {
            spec = spec.env(k, v);
        }
        let path = env.iter().find(|(k, _)| k == "PATH");
        let path = path.map_or(base_path.clone(), |(_, p)| format!("{p}:{base_path}"));
        self.spawn_and_capture(&spec.env("PATH", path), checkout)
            .await
    }

    async fn spawn_and_capture(
        &self,
        spec: &SandboxSpec,
        checkout: &Checkout,
    ) -> Result<CheckOutput> {
        let mut child = self.sandbox.spawn(spec).await?;
        let (stdout, stderr) = output::capture(&mut child, OUTPUT_LIMIT).await;
        let exit = child.wait().await;
        checkout.restore_git();
        let exit = exit?;
        Ok(CheckOutput {
            exit,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

fn checkout_spec(program: PathBuf, checkout: &Checkout, limits: Limits) -> SandboxSpec {
    let spec = SandboxSpec::new(program)
        .rw_path(checkout.root())
        .cwd(checkout.root())
        .timeout(limits.timeout)
        .memory_limit_bytes(limits.memory_limit_mb.saturating_mul(1024 * 1024));
    checkout
        .git_paths()
        .iter()
        .fold(spec, |spec, p| spec.ro_path(p))
}

fn shell_program() -> Result<PathBuf> {
    resolve_program("sh").or_else(|e| {
        Path::new("/bin/sh")
            .canonicalize()
            .with_context(|| format!("{e:#}; /bin/sh is not usable either"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_spec_binds_git_read_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let checkout = Checkout::resolve(dir.path());
        let spec = checkout_spec("/bin/sh".into(), &checkout, Limits::default());
        assert_eq!(spec.rw_paths, vec![dir.path().to_path_buf()]);
        assert_eq!(spec.ro_paths, vec![dir.path().join(".git")]);
        assert_eq!(spec.network, Network::None);
    }
}
