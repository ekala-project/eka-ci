use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::net::{Network, RESOLV_FD};

pub(crate) const NIX_DAEMON_SOCKET_DIR: &str = "/nix/var/nix/daemon-socket";

pub(crate) const SYSTEM_RO_PATHS: &[&str] = &[
    "/nix/store",
    "/etc/ssl/certs",
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/passwd",
    "/etc/group",
    "/etc/static",
];

pub(crate) const SANDBOX_HOME: &str = "/tmp/home";

#[derive(Debug, Clone)]
pub struct SandboxSpec {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) ro_paths: Vec<PathBuf>,
    pub(crate) rw_paths: Vec<PathBuf>,
    pub(crate) env: Vec<(String, String)>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) memory_limit_bytes: Option<u64>,
    pub(crate) network: Network,
    pub(crate) probe: bool,
}

impl SandboxSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            ro_paths: Vec::new(),
            rw_paths: Vec::new(),
            env: Vec::new(),
            cwd: None,
            timeout: None,
            memory_limit_bytes: None,
            network: Network::None,
            probe: false,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn ro_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.ro_paths.push(path.into());
        self
    }

    pub fn rw_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.rw_paths.push(path.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn memory_limit_bytes(mut self, bytes: u64) -> Self {
        self.memory_limit_bytes = Some(bytes);
        self
    }

    pub fn network(mut self, network: Network) -> Self {
        self.network = network;
        self
    }

    pub(crate) fn network_value(&self) -> Network {
        self.network
    }

    pub(crate) fn timeout_value(&self) -> Option<Duration> {
        self.timeout
    }

    pub(crate) fn bwrap_args(&self, helper: &Path) -> Vec<OsString> {
        let mut out = self.bwrap_prefix(helper);
        out.push(helper.into());
        if self.probe {
            out.push("--probe".into());
        } else {
            out.extend(self.helper_args());
        }
        out
    }

    fn bwrap_prefix(&self, helper: &Path) -> Vec<OsString> {
        let mut out: Vec<OsString> = vec!["--unshare-all".into()];
        out.extend(
            [
                "--clearenv",
                "--die-with-parent",
                "--new-session",
                "--proc",
                "/proc",
                "--dev",
                "/dev",
                "--tmpfs",
                "/tmp",
                "--dir",
                SANDBOX_HOME,
            ]
            .iter()
            .map(OsString::from),
        );
        self.push_mounts(&mut out, helper);
        self.push_env(&mut out);
        if let Some(cwd) = &self.cwd {
            push_all(&mut out, ["--chdir".into(), cwd.clone().into()]);
        }
        out.push("--".into());
        out
    }

    fn push_mounts(&self, out: &mut Vec<OsString>, helper: &Path) {
        let filtered = self.network == Network::Filtered;
        for p in SYSTEM_RO_PATHS {
            if filtered && *p == "/etc/resolv.conf" {
                continue;
            }
            push_all(out, ["--ro-bind-try".into(), (*p).into(), (*p).into()]);
        }
        // The host resolver may sit on its loopback; pasta forwards DNS_ADDR to it.
        if filtered {
            push_all(
                out,
                [
                    "--ro-bind-data".into(),
                    RESOLV_FD.to_string().into(),
                    "/etc/resolv.conf".into(),
                ],
            );
        }
        push_all(
            out,
            [
                "--ro-bind-try".into(),
                NIX_DAEMON_SOCKET_DIR.into(),
                NIX_DAEMON_SOCKET_DIR.into(),
            ],
        );
        for bin in [helper, self.program.as_path()] {
            if !bin.starts_with("/nix/store") {
                push_all(out, ["--ro-bind".into(), bin.into(), bin.into()]);
            }
        }
        let mut user: Vec<(&PathBuf, &str)> = self
            .ro_paths
            .iter()
            .map(|p| (p, "--ro-bind"))
            .chain(self.rw_paths.iter().map(|p| (p, "--bind")))
            .collect();
        // Parents first, so a read-only `.git` lands on top of its read-write checkout.
        user.sort_by_key(|(p, _)| p.components().count());
        for (p, flag) in user {
            push_all(out, [flag.into(), p.into(), p.into()]);
        }
    }

    fn program_outside_store(&self) -> Option<PathBuf> {
        let outside = !self.program.starts_with("/nix/store");
        outside.then(|| self.program.clone())
    }

    fn push_env(&self, out: &mut Vec<OsString>) {
        let path = self
            .program
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let base = [
            ("PATH", path.as_str()),
            ("HOME", SANDBOX_HOME),
            ("TMPDIR", "/tmp"),
            ("NIX_REMOTE", "daemon"),
        ];
        for (k, v) in base {
            push_all(out, ["--setenv".into(), k.into(), v.into()]);
        }
        for (k, v) in &self.env {
            push_all(out, ["--setenv".into(), k.into(), v.into()]);
        }
    }

    fn helper_args(&self) -> Vec<OsString> {
        let mut out = Vec::new();
        let ro = SYSTEM_RO_PATHS
            .iter()
            .map(PathBuf::from)
            .chain(["/proc".into()])
            .chain(self.ro_paths.iter().cloned())
            .chain(self.program_outside_store());
        for p in ro {
            push_all(&mut out, ["--ro".into(), p.into()]);
        }
        let rw = ["/tmp", "/dev"]
            .iter()
            .map(PathBuf::from)
            .chain(self.rw_paths.iter().cloned());
        for p in rw {
            push_all(&mut out, ["--rw".into(), p.into()]);
        }
        push_all(
            &mut out,
            ["--socket-dir".into(), NIX_DAEMON_SOCKET_DIR.into()],
        );
        if let Some(bytes) = self.memory_limit_bytes {
            push_all(&mut out, ["--mem".into(), bytes.to_string().into()]);
        }
        out.push("--".into());
        out.push(self.program.clone().into());
        out.extend(self.args.iter().cloned());
        out
    }
}

fn push_all<const N: usize>(out: &mut Vec<OsString>, items: [OsString; N]) {
    out.extend(items);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(args: &[OsString]) -> String {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn sample() -> SandboxSpec {
        SandboxSpec::new("/nix/store/abc-tool/bin/tool")
            .arg("--flag")
            .ro_path("/work")
            .rw_path("/scratch")
            .env("FOO", "bar")
            .cwd("/work")
            .memory_limit_bytes(1024)
    }

    #[test]
    fn bwrap_args_isolate_and_clear_env() {
        let s = joined(&sample().bwrap_args(Path::new("/nix/store/h/bin/helper")));
        for needle in [
            "--unshare-all --clearenv --die-with-parent --new-session",
            "--ro-bind-try /nix/store /nix/store",
            "--ro-bind /work /work",
            "--bind /scratch /scratch",
            "--setenv PATH /nix/store/abc-tool/bin",
            "--setenv NIX_REMOTE daemon",
            "--setenv FOO bar",
            "--chdir /work",
        ] {
            assert!(s.contains(needle), "missing `{needle}` in `{s}`");
        }
    }

    #[test]
    fn helper_receives_same_paths_and_command() {
        let s = joined(&sample().bwrap_args(Path::new("/nix/store/h/bin/helper")));
        let (_, helper) = s.split_once("-- /nix/store/h/bin/helper ").unwrap();
        for needle in ["--ro /work", "--rw /scratch", "--rw /tmp", "--mem 1024"] {
            assert!(helper.contains(needle), "missing `{needle}` in `{helper}`");
        }
        assert!(helper.ends_with("-- /nix/store/abc-tool/bin/tool --flag"));
    }

    #[test]
    fn filtered_network_replaces_resolv_conf() {
        let helper = Path::new("/nix/store/h/bin/helper");
        let s = joined(&sample().network(Network::Filtered).bwrap_args(helper));
        assert!(s.starts_with("--unshare-all --clearenv"), "{s}");
        assert!(!s.contains("--share-net"), "{s}");
        assert!(s.contains("--ro-bind-data 3 /etc/resolv.conf"), "{s}");
        assert!(!s.contains("--ro-bind-try /etc/resolv.conf"), "{s}");
        let none = joined(&sample().bwrap_args(helper));
        assert!(none.starts_with("--unshare-all --clearenv") && !none.contains("--ro-bind-data"));
    }

    #[test]
    fn nested_read_only_path_is_mounted_after_its_parent() {
        let spec = SandboxSpec::new("/nix/store/abc-tool/bin/tool")
            .ro_path("/work/.git")
            .rw_path("/work");
        let s = joined(&spec.bwrap_args(Path::new("/nix/store/h/bin/helper")));
        let parent = s.find("--bind /work /work").unwrap();
        let child = s.find("--ro-bind /work/.git /work/.git").unwrap();
        assert!(parent < child, "{s}");
    }

    #[test]
    fn helper_outside_store_is_bound() {
        let s = joined(&sample().bwrap_args(Path::new("/target/debug/helper")));
        assert!(s.contains("--ro-bind /target/debug/helper /target/debug/helper"));
    }
}
