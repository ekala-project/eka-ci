use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::oneshot;
use tracing::warn;

const FALLBACK_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxExit {
    Exited(ExitStatus),
    TimedOut(Duration),
}

type Pipes = (
    Option<std::process::ChildStdout>,
    Option<std::process::ChildStderr>,
);

pub struct SandboxedChild {
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    kill: UnixStream,
    exit: Option<oneshot::Receiver<io::Result<SandboxExit>>>,
}

impl SandboxedChild {
    pub fn kill(&self) {
        (&self.kill).write_all(&[1]).ok();
    }

    pub async fn wait(&mut self) -> Result<SandboxExit> {
        let rx = self.exit.take().context("sandbox exit already awaited")?;
        rx.await
            .context("sandbox supervisor thread vanished")?
            .context("failed to wait for sandbox")
    }
}

pub(crate) async fn spawn_supervised(
    mut cmd: Command,
    timeout: Option<Duration>,
) -> Result<SandboxedChild> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (kill, kill_rx) = UnixStream::pair().context("failed to create sandbox kill socket")?;
    let (ready_tx, ready_rx) = oneshot::channel::<io::Result<Pipes>>();
    let (exit_tx, exit_rx) = oneshot::channel();
    // PR_SET_PDEATHSIG fires when the forking *thread* exits, so each sandbox gets its own.
    std::thread::Builder::new()
        .name("ekaci-sandbox".into())
        .spawn(move || {
            let outcome = supervise(cmd, timeout, &kill_rx, ready_tx);
            if let Some(outcome) = outcome {
                exit_tx.send(outcome).ok();
            }
        })
        .context("failed to start sandbox supervisor thread")?;

    let (stdout, stderr) = ready_rx
        .await
        .context("sandbox supervisor thread vanished")?
        .context("failed to spawn bwrap")?;
    Ok(SandboxedChild {
        stdout: stdout.map(ChildStdout::from_std).transpose()?,
        stderr: stderr.map(ChildStderr::from_std).transpose()?,
        kill,
        exit: Some(exit_rx),
    })
}

fn supervise(
    mut cmd: Command,
    timeout: Option<Duration>,
    kill: &UnixStream,
    ready_tx: oneshot::Sender<io::Result<Pipes>>,
) -> Option<io::Result<SandboxExit>> {
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            ready_tx.send(Err(e)).ok();
            return None;
        },
    };
    let pipes = (child.stdout.take(), child.stderr.take());
    ready_tx.send(Ok(pipes)).ok();
    Some(watch(&mut child, timeout, kill))
}

// Dropping the SandboxedChild closes `kill`, which wakes the poll just like an explicit kill.
fn watch(
    child: &mut Child,
    timeout: Option<Duration>,
    kill: &UnixStream,
) -> io::Result<SandboxExit> {
    let deadline = timeout.map(|t| Instant::now() + t);
    let exited = pidfd_open(child.id());
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(SandboxExit::Exited(status));
        }
        let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        if let (Some(Duration::ZERO), Some(timeout)) = (left, timeout) {
            kill_and_reap(child)?;
            return Ok(SandboxExit::TimedOut(timeout));
        }
        let fallback = exited.is_none().then_some(FALLBACK_POLL);
        let wait = left.into_iter().chain(fallback).min();
        if wait_readable(kill, exited.as_ref(), wait)? {
            return kill_and_reap(child).map(SandboxExit::Exited);
        }
    }
}

// pidfd_open needs Linux 5.3; without it the exit is noticed by polling.
fn pidfd_open(pid: u32) -> Option<OwnedFd> {
    // SAFETY: pidfd_open takes a pid and flags and returns a new fd or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if fd < 0 {
        warn!(
            "pidfd_open failed ({}); polling for sandbox exit",
            io::Error::last_os_error()
        );
        return None;
    }
    // SAFETY: the syscall returned a new fd that nothing else owns.
    Some(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

/// Blocks until the child exits, a kill is requested or `wait` elapses; true on a kill.
fn wait_readable(
    kill: &UnixStream,
    exited: Option<&OwnedFd>,
    wait: Option<Duration>,
) -> io::Result<bool> {
    let pollfd = |fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let mut fds = vec![pollfd(kill.as_raw_fd())];
    fds.extend(exited.map(|fd| pollfd(fd.as_raw_fd())));
    let ms = wait.map_or(-1, |d| {
        d.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
    });
    // SAFETY: `fds` is a valid array of `fds.len()` pollfd structs.
    if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) } < 0 {
        let e = io::Error::last_os_error();
        return if e.kind() == io::ErrorKind::Interrupted {
            Ok(false)
        } else {
            Err(e)
        };
    }
    Ok(fds[0].revents != 0)
}

fn kill_and_reap(child: &mut Child) -> io::Result<ExitStatus> {
    if let Err(e) = child.kill() {
        warn!("failed to kill sandbox (may already have exited): {e}");
    }
    child.wait()
}

#[cfg(test)]
mod tests {
    use tokio::io::AsyncReadExt;

    use super::*;

    #[tokio::test]
    async fn captures_output_and_status() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo out; echo err >&2; exit 3"]);
        let mut child = spawn_supervised(cmd, None).await.unwrap();
        let mut out = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .await
            .unwrap();
        let mut err = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut err)
            .await
            .unwrap();
        let exit = child.wait().await.unwrap();
        assert_eq!((out.as_str(), err.as_str()), ("out\n", "err\n"));
        assert!(matches!(exit, SandboxExit::Exited(s) if s.code() == Some(3)));
    }

    #[tokio::test]
    async fn timeout_kills_the_process() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30"]);
        let started = Instant::now();
        let mut child = spawn_supervised(cmd, Some(Duration::from_millis(200)))
            .await
            .unwrap();
        let exit = child.wait().await.unwrap();
        assert_eq!(exit, SandboxExit::TimedOut(Duration::from_millis(200)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn explicit_kill_terminates() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30"]);
        let mut child = spawn_supervised(cmd, None).await.unwrap();
        child.kill();
        let exit = child.wait().await.unwrap();
        assert!(matches!(exit, SandboxExit::Exited(s) if !s.success()));
    }

    #[tokio::test]
    async fn dropping_the_child_kills_it() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo $$; exec sleep 30"]);
        let mut child = spawn_supervised(cmd, None).await.unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let mut pid = String::new();
        while !pid.ends_with('\n') {
            let mut b = [0u8; 1];
            stdout.read_exact(&mut b).await.unwrap();
            pid.push(b[0] as char);
        }
        let proc = std::path::PathBuf::from(format!("/proc/{}", pid.trim()));
        assert!(proc.exists());
        drop(child);
        let deadline = Instant::now() + Duration::from_secs(5);
        while proc.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!proc.exists());
    }
}
