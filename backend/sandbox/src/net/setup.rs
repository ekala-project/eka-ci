use std::ffi::{CString, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::netlink::Netlink;
use super::{GATEWAY, IFNAME, Ipv4Cidr, Route};

pub(crate) const FLAG: &str = "--net-setup";
pub(crate) const JOINED: &str = "joined";
pub(crate) const ROUTED: &str = "routed";

const READY_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct NetArgs {
    pub routes: Vec<Route>,
    pub userns: Option<PathBuf>,
    pub netns: Option<PathBuf>,
    pub pasta: Option<PathBuf>,
    pub command: Vec<OsString>,
}

pub(crate) fn parse_args(args: Vec<OsString>) -> Result<NetArgs> {
    let mut out = NetArgs::default();
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let flag = flag.to_string_lossy().into_owned();
        if flag == "--" {
            out.command = it.collect();
            break;
        }
        let value = it
            .next()
            .with_context(|| format!("missing value for `{flag}`"))?;
        let value = value.to_string_lossy().into_owned();
        match flag.as_str() {
            "--unreachable" => out.routes.push(Route::Unreachable(value.parse()?)),
            "--via" => out.routes.push(Route::ViaGateway(value.parse()?)),
            "--userns" => out.userns = Some(value.into()),
            "--netns" => out.netns = Some(value.into()),
            "--pasta" => out.pasta = Some(value.into()),
            other => bail!("unknown network helper argument `{other}`"),
        }
    }
    Ok(out)
}

pub(crate) fn route_args(plan: &[Route]) -> Vec<OsString> {
    let mut out = Vec::new();
    for route in plan {
        let (flag, cidr): (&str, &Ipv4Cidr) = match route {
            Route::Unreachable(c) => ("--unreachable", c),
            Route::ViaGateway(c) => ("--via", c),
        };
        out.extend([flag.into(), cidr.to_string().into()]);
    }
    out
}

pub(crate) fn run(args: Vec<OsString>) -> Result<()> {
    let args = parse_args(args)?;
    if let (Some(user), Some(net)) = (&args.userns, &args.netns) {
        let user = File::open(user).context("open sandbox user ns")?;
        let net = File::open(net).context("open sandbox net ns")?;
        enter(&user, libc::CLONE_NEWUSER).context("setns(user)")?;
        enter(&net, libc::CLONE_NEWNET).context("setns(net)")?;
    }
    report(JOINED)?;
    let ifindex = wait_ready()?;
    let mut nl = Netlink::open().context("failed to open rtnetlink socket")?;
    for route in &args.routes {
        nl.replace(*route, ifindex, GATEWAY)
            .with_context(|| format!("failed to install route {route:?}"))?;
    }
    report(ROUTED)?;
    let mut rest = Vec::new();
    std::io::stdin()
        .read_to_end(&mut rest)
        .context("wait for sandbox exit")?;
    Ok(())
}

fn report(event: &str) -> Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{event}")
        .and_then(|()| out.flush())
        .context("report to attach helper")
}

fn enter(ns: &File, kind: libc::c_int) -> std::io::Result<()> {
    // SAFETY: `ns` is an open namespace file descriptor.
    if unsafe { libc::setns(ns.as_raw_fd(), kind) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// pasta adds the default route last; once it exists the device is ready.
fn wait_ready() -> Result<u32> {
    let name = CString::new(IFNAME).expect("IFNAME has no NUL");
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        // SAFETY: `name` is a valid NUL-terminated string.
        let ifindex = unsafe { libc::if_nametoindex(name.as_ptr()) };
        let routes = std::fs::read_to_string("/proc/self/net/route").unwrap_or_default();
        if ifindex != 0 && has_default_route(&routes) {
            return Ok(ifindex);
        }
        if Instant::now() >= deadline {
            bail!("pasta did not configure {IFNAME} within {READY_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn has_default_route(proc_net_route: &str) -> bool {
    proc_net_route.lines().skip(1).any(|l| {
        let mut cols = l.split_whitespace();
        cols.next() == Some(IFNAME) && cols.next() == Some("00000000")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_and_rejects() {
        let a = parse_args(os(&[
            "--userns",
            "/u",
            "--netns",
            "/n",
            "--pasta",
            "/p",
            "--via",
            "0.0.0.0/0",
            "--",
            "/bwrap",
            "-x",
        ]))
        .unwrap();
        assert_eq!(a.userns, Some(PathBuf::from("/u")));
        assert_eq!(a.netns, Some(PathBuf::from("/n")));
        assert_eq!(a.pasta, Some(PathBuf::from("/p")));
        assert_eq!(a.command, os(&["/bwrap", "-x"]));
        assert!(parse_args(os(&["--via", "10.0.0.1/8"])).is_err());
        assert!(parse_args(os(&["--bogus", "x"])).is_err());
        assert!(parse_args(os(&["--via"])).is_err());
    }

    #[test]
    fn route_args_round_trip() {
        let plan = vec![
            Route::Unreachable("10.0.0.0/8".parse().unwrap()),
            Route::ViaGateway("0.0.0.0/0".parse().unwrap()),
        ];
        assert_eq!(parse_args(route_args(&plan)).unwrap().routes, plan);
    }

    #[test]
    fn detects_default_route() {
        let table = concat!(
            "Iface\tDestination\tGateway\n",
            "ekaci0\t0002000C0\t00000000\n",
            "ekaci0\t00000000\t010200C0\n",
        );
        assert!(has_default_route(table));
        assert!(!has_default_route("Iface\tDestination\neth0\t00000000\n"));
    }
}
