use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::setup::{self, NetArgs};
use super::{DNS_ADDR, RESOLV_FD, pasta_args};

pub(crate) const FLAG: &str = "--net-attach";

const INFO_FD: RawFd = 4;
const BLOCK_FD: RawFd = 5;
const MAP_TIMEOUT: Duration = Duration::from_secs(10);

// The sandbox stays blocked until pasta is attached and the routes are installed.
pub(crate) fn run(args: Vec<OsString>) -> Result<i32> {
    let args = setup::parse_args(args)?;
    let pasta = args.pasta.clone().context("--pasta is required")?;
    if args.command.is_empty() {
        bail!("no bwrap command given after `--`");
    }
    let (info_r, info_w) = pipe()?;
    let (block_r, block_w) = pipe()?;
    let resolv = resolv_conf()?;
    let mut bwrap = Reaped(spawn_bwrap(&args, [&resolv, &info_w, &block_r])?);
    drop((info_w, block_r, resolv));
    let _network = attach(&args, &pasta, info_r)?;
    File::from(block_w)
        .write_all(b"x")
        .context("unblock sandbox")?;
    let status = bwrap.0.wait().context("wait for bwrap")?;
    Ok(exit_code(status))
}

struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        if let Err(e) = self.0.kill() {
            eprintln!("ekaci-sandbox-helper: kill {}: {e}", self.0.id());
        }
        if let Err(e) = self.0.wait() {
            eprintln!("ekaci-sandbox-helper: wait {}: {e}", self.0.id());
        }
    }
}

// Reaping pasta after SIGKILL takes tens of ms; the helper exits right after, so it is skipped.
struct Killed(Child, Option<std::thread::JoinHandle<std::io::Result<u64>>>);

impl Killed {
    fn drain(mut self) {
        self.0.kill().ok();
        if let Err(e) = self.0.wait() {
            eprintln!("ekaci-sandbox-helper: wait {}: {e}", self.0.id());
        }
        if let Some(relay) = self.1.take() {
            relay.join().ok();
        }
    }
}

impl Drop for Killed {
    fn drop(&mut self) {
        if let Err(e) = self.0.kill() {
            eprintln!("ekaci-sandbox-helper: kill {}: {e}", self.0.id());
        }
    }
}

// pasta quits when its target pid exits, so the setup process stays alive as one.
fn attach(args: &NetArgs, pasta: &Path, info_r: OwnedFd) -> Result<(Killed, Killed)> {
    let pid = child_pid(info_r)?;
    wait_for_id_maps(pid)?;
    let net = File::open(format!("/proc/{pid}/ns/net")).context("open sandbox net ns")?;
    let owner = owning_userns(&net)?;
    let exe = std::env::current_exe().context("locate helper")?;
    let mut cmd = Command::new(exe);
    cmd.arg(setup::FLAG)
        .args(["--netns".into(), fd_path(net.as_raw_fd())])
        .args(["--userns".into(), fd_path(owner.as_raw_fd())])
        .args(setup::route_args(&args.routes))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut holder = Killed(
        with_pdeathsig(&mut cmd)
            .spawn()
            .context("spawn network setup")?,
        None,
    );
    let mut events = std::io::BufReader::new(holder.0.stdout.take().context("setup stdout")?);
    // As root, pasta would switch to `nobody` and lose access to the holder.
    // SAFETY: getuid/getgid cannot fail.
    let runas = unsafe { format!("{}:{}", libc::getuid(), libc::getgid()) };
    let started = expect_line(&mut events, setup::JOINED).and_then(|()| {
        let mut pasta = Command::new(pasta);
        let cmd = with_pdeathsig(
            pasta
                .args(pasta_args())
                .args(["--runas", &runas])
                .arg(holder.0.id().to_string()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null());
        spawn_relayed(cmd).context("spawn pasta")
    });
    let pasta = started?;
    if let Err(e) = expect_line(&mut events, setup::ROUTED) {
        pasta.drain();
        return Err(e);
    }
    Ok((pasta, holder))
}

// An inherited stderr would reach EOF for the caller only once pasta finishes dying after
// SIGKILL, tens of ms later; it is relayed by a thread that ends with the helper instead.
fn spawn_relayed(cmd: &mut Command) -> Result<Killed> {
    let mut child = cmd.stderr(Stdio::piped()).spawn()?;
    let mut stderr = child.stderr.take().context("child stderr")?;
    let relay = std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::stderr()));
    Ok(Killed(child, Some(relay)))
}

fn expect_line(events: &mut impl std::io::BufRead, want: &str) -> Result<()> {
    let mut line = String::new();
    events.read_line(&mut line).context("read network setup")?;
    if line.trim_end() != want {
        bail!("network setup failed before `{want}`");
    }
    Ok(())
}

fn spawn_bwrap(args: &NetArgs, fds: [&OwnedFd; 3]) -> Result<Child> {
    let [resolv, info, block] = fds.map(|fd| high_dup(fd.as_raw_fd()));
    let (resolv, info, block) = (resolv?, info?, block?);
    let mut cmd = Command::new(&args.command[0]);
    cmd.args([
        "--info-fd",
        &INFO_FD.to_string(),
        "--block-fd",
        &BLOCK_FD.to_string(),
    ])
    .args(&args.command[1..])
    .stdin(Stdio::null());
    let raw = [
        (resolv.as_raw_fd(), RESOLV_FD),
        (info.as_raw_fd(), INFO_FD),
        (block.as_raw_fd(), BLOCK_FD),
    ];
    // SAFETY: only async-signal-safe calls (dup2) between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            for (from, to) in raw {
                if libc::dup2(from, to) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    let child = with_pdeathsig(&mut cmd).spawn().context("spawn bwrap")?;
    Ok(child)
}

fn with_pdeathsig(cmd: &mut Command) -> &mut Command {
    // SAFETY: prctl is async-signal-safe.
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        })
    }
}

fn child_pid(info: OwnedFd) -> Result<i32> {
    let mut json = String::new();
    File::from(info)
        .read_to_string(&mut json)
        .context("read bwrap info")?;
    parse_child_pid(&json).with_context(|| format!("no child-pid in bwrap info {json:?}"))
}

// bwrap reports the pid before the sandbox has written its id maps.
fn wait_for_id_maps(pid: i32) -> Result<()> {
    let deadline = Instant::now() + MAP_TIMEOUT;
    let mapped = |f: &str| {
        std::fs::read_to_string(format!("/proc/{pid}/{f}")).is_ok_and(|m| !m.trim().is_empty())
    };
    while !(mapped("uid_map") && mapped("gid_map")) {
        if Instant::now() >= deadline {
            bail!("sandbox {pid} did not set up its user namespace within {MAP_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

// With --dev the sandbox moves into a nested userns; the netns belongs to its parent.
fn owning_userns(net: &File) -> Result<OwnedFd> {
    // SAFETY: NS_GET_USERNS on a namespace fd returns a new fd or -1.
    let fd = unsafe { libc::ioctl(net.as_raw_fd(), libc::NS_GET_USERNS) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error()).context("ioctl(NS_GET_USERNS)");
    }
    // SAFETY: `fd` was just created and is owned here.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn fd_path(fd: RawFd) -> OsString {
    format!("/proc/{}/fd/{fd}", std::process::id()).into()
}

fn parse_child_pid(json: &str) -> Option<i32> {
    let rest = &json[json.find("\"child-pid\"")? + "\"child-pid\"".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn resolv_conf() -> Result<OwnedFd> {
    let (r, w) = pipe()?;
    File::from(w)
        .write_all(format!("nameserver {DNS_ADDR}\n").as_bytes())
        .context("write resolv.conf pipe")?;
    Ok(r)
}

fn pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 returns.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error()).context("pipe2");
    }
    // SAFETY: both descriptors were just created and are owned here.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn high_dup(fd: RawFd) -> Result<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC on a valid descriptor returns a new one.
    let new = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 10) };
    if new < 0 {
        return Err(std::io::Error::last_os_error()).context("fcntl(F_DUPFD_CLOEXEC)");
    }
    // SAFETY: `new` was just created and is owned here.
    Ok(unsafe { OwnedFd::from_raw_fd(new) })
}

fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|s| 128 + s))
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bwrap_info() {
        assert_eq!(
            parse_child_pid("{\n    \"child-pid\": 1234\n}\n"),
            Some(1234)
        );
        assert_eq!(parse_child_pid("{\"child-pid\":7,\"x\":1}"), Some(7));
        assert_eq!(parse_child_pid("{}"), None);
    }
}
