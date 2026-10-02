use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use landlock::{
    ABI, Access, AccessFs, RestrictionStatus, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus, Scope, path_beneath_rules,
};

const TARGET_ABI: ABI = ABI::V9;

pub const PROBE_PREFIX: &str = "ekaci-sandbox-probe landlock=";

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct HelperArgs {
    pub ro: Vec<PathBuf>,
    pub rw: Vec<PathBuf>,
    pub socket_dirs: Vec<PathBuf>,
    pub mem_bytes: Option<u64>,
    pub probe: bool,
    pub command: Vec<OsString>,
}

pub(crate) fn parse_args(args: Vec<OsString>) -> Result<HelperArgs> {
    let mut out = HelperArgs::default();
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let flag = flag.to_string_lossy().into_owned();
        match flag.as_str() {
            "--" => {
                out.command = it.collect();
                break;
            },
            "--probe" => out.probe = true,
            "--ro" => out.ro.push(next_value(&mut it, &flag)?.into()),
            "--rw" => out.rw.push(next_value(&mut it, &flag)?.into()),
            "--socket-dir" => out.socket_dirs.push(next_value(&mut it, &flag)?.into()),
            "--mem" => {
                let raw = next_value(&mut it, &flag)?;
                let bytes = raw
                    .to_string_lossy()
                    .parse()
                    .context("invalid --mem value")?;
                out.mem_bytes = Some(bytes);
            },
            other => bail!("unknown helper argument `{other}`"),
        }
    }
    if !out.probe && out.command.is_empty() {
        bail!("no command given after `--`");
    }
    Ok(out)
}

fn next_value(it: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<OsString> {
    it.next()
        .with_context(|| format!("missing value for `{flag}`"))
}

pub fn run(args: Vec<OsString>) -> Result<()> {
    match args.first().and_then(|a| a.to_str()) {
        Some(crate::net::setup::FLAG) => return crate::net::setup::run(args[1..].to_vec()),
        Some(crate::net::attach::FLAG) => {
            std::process::exit(crate::net::attach::run(args[1..].to_vec())?)
        },
        _ => {},
    }
    let args = parse_args(args)?;
    if let Some(bytes) = args.mem_bytes {
        set_address_space_limit(bytes)?;
    }
    // Landlock forbids mount(2), hence applied here, after bwrap built the mounts.
    let status = restrict(&args)?;
    let label = status_label(&status);
    if args.probe {
        println!("{PROBE_PREFIX}{label}");
        return Ok(());
    }
    let mut cmd = std::process::Command::new(&args.command[0]);
    cmd.args(&args.command[1..]);
    let err = cmd.exec();
    Err(err).with_context(|| format!("failed to exec {:?}", args.command[0]))
}

fn status_label(status: &RestrictionStatus) -> &'static str {
    match status.ruleset {
        RulesetStatus::FullyEnforced => "full",
        RulesetStatus::PartiallyEnforced => "partial",
        RulesetStatus::NotEnforced => "none",
    }
}

fn restrict(args: &HelperArgs) -> Result<RestrictionStatus> {
    let read = AccessFs::from_read(TARGET_ABI);
    let all = AccessFs::from_all(TARGET_ABI);
    let socket = read | AccessFs::ResolveUnix;
    Ruleset::default()
        .handle_access(all)?
        .scope(Scope::from_all(TARGET_ABI))?
        .create()?
        .add_rules(path_beneath_rules(&args.ro, read))?
        .add_rules(path_beneath_rules(&args.rw, all))?
        .add_rules(path_beneath_rules(&args.socket_dirs, socket))?
        // Nix walks paths from an fd for `/`; listing shows only what bwrap mounted.
        .add_rules(path_beneath_rules(["/"], AccessFs::ReadDir))?
        .restrict_self()
        .context("failed to apply landlock ruleset")
}

fn set_address_space_limit(bytes: u64) -> Result<()> {
    let lim = libc::rlimit {
        rlim_cur: bytes as libc::rlim_t,
        rlim_max: bytes as libc::rlim_t,
    };
    // SAFETY: `setrlimit` only reads the struct we pass by pointer.
    let rc = unsafe { libc::setrlimit(libc::RLIMIT_AS, &lim) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()).context("setrlimit(RLIMIT_AS) failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_full_invocation() {
        let a = parse_args(os(&[
            "--ro",
            "/a",
            "--rw",
            "/b",
            "--socket-dir",
            "/s",
            "--mem",
            "42",
            "--",
            "/bin/x",
            "--ro",
        ]))
        .unwrap();
        assert_eq!(a.ro, vec![PathBuf::from("/a")]);
        assert_eq!(a.rw, vec![PathBuf::from("/b")]);
        assert_eq!(a.socket_dirs, vec![PathBuf::from("/s")]);
        assert_eq!(a.mem_bytes, Some(42));
        assert_eq!(a.command, os(&["/bin/x", "--ro"]));
    }

    #[test]
    fn rejects_missing_command_and_unknown_flags() {
        assert!(parse_args(os(&["--ro", "/a"])).is_err());
        assert!(parse_args(os(&["--bogus", "--", "/x"])).is_err());
        assert!(parse_args(os(&["--mem", "lots", "--", "/x"])).is_err());
        assert!(parse_args(os(&["--probe"])).unwrap().probe);
    }
}
