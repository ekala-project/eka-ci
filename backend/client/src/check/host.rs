use std::ffi::OsString;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sandbox::checkout::Checkout;
use sandbox::devshell::{self, DevShell};
use tokio::process::Command;
use tracing::debug;

pub enum Launcher {
    Direct,
    #[cfg(target_os = "macos")]
    Seatbelt(super::seatbelt::Seatbelt),
}

pub struct HostOutput {
    pub exit_code: i32,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Launcher {
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    fn command(
        &self,
        checkout: &Checkout,
        network: bool,
        program: &str,
        args: &[OsString],
    ) -> Command {
        let mut cmd = match self {
            Launcher::Direct => {
                let mut cmd = Command::new(program);
                cmd.args(args);
                cmd
            },
            #[cfg(target_os = "macos")]
            Launcher::Seatbelt(sb) => sb.command(checkout, network, program, args),
        };
        cmd.current_dir(checkout.root());
        cmd
    }
}

pub async fn dev_shell_env(
    launcher: &Launcher,
    checkout: &Checkout,
    shell: DevShell<'_>,
    timeout: Duration,
) -> Result<Vec<(String, String)>> {
    devshell::require_entry_file(shell, checkout.root())?;
    let (program, args) = shell.command();
    debug!("Running {program} {args:?}");
    let mut cmd = launcher.command(checkout, true, program, &args);
    if matches!(launcher, Launcher::Direct) {
        cmd.env_clear();
    }
    let out = output_with_timeout(cmd, timeout).await?;
    if out.timed_out {
        bail!("{program} timed out after {timeout:?}: {}", out.stderr);
    }
    if out.exit_code != 0 {
        bail!("{program} failed ({}): {}", out.exit_code, out.stderr);
    }
    devshell::parse_env(out.stdout.as_bytes())
}

pub async fn run(
    launcher: &Launcher,
    checkout: &Checkout,
    command: &str,
    allow_network: bool,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<HostOutput> {
    debug!(
        "Executing command in {}: {}",
        checkout.root().display(),
        command
    );
    let args = ["-c".into(), command.into()];
    let mut cmd = launcher.command(checkout, allow_network, "sh", &args);
    cmd.envs(env.iter().map(|(k, v)| (k, v)));
    output_with_timeout(cmd, timeout).await
}

async fn output_with_timeout(mut cmd: Command, timeout: Duration) -> Result<HostOutput> {
    cmd.process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().context("failed to spawn command")?;
    let pgid = child.id();
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(out) => {
            let out = out.context("failed to wait for command")?;
            Ok(HostOutput {
                exit_code: out.status.code().unwrap_or(-1),
                timed_out: false,
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            })
        },
        Err(_) => {
            if let Some(pgid) = pgid {
                kill_group(pgid);
            }
            Ok(HostOutput {
                exit_code: -1,
                timed_out: true,
                stdout: String::new(),
                stderr: sandbox::timeout_note(timeout),
            })
        },
    }
}

fn kill_group(pgid: u32) {
    // SAFETY: killpg only takes integer arguments.
    let rc = unsafe { libc::killpg(pgid as libc::pid_t, libc::SIGKILL) };
    if rc != 0 {
        debug!("killpg({pgid}) failed: {}", std::io::Error::last_os_error());
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[tokio::test]
    async fn timeout_kills_process_group() {
        let started = Instant::now();
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "sleep 30 & sleep 30"]);
        let out = output_with_timeout(cmd, Duration::from_millis(300))
            .await
            .unwrap();
        assert!(out.timed_out && out.exit_code == -1);
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
