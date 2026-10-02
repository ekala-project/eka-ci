use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sandbox::checkout::Checkout;
use tokio::process::Command;

const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
const NIX_DAEMON_SOCKET: &str = "/nix/var/nix/daemon-socket/socket";

const BASE_PROFILE: &str = r#"(version 1)
(deny default)
(allow process-fork)
(allow process-exec)
(allow signal (target same-sandbox))
(allow process-info* (target same-sandbox))
(allow sysctl-read)
(allow user-preference-read)
(allow ipc-posix-sem)
(allow ipc-posix-shm)
(allow pseudo-tty)
(allow file-ioctl (subpath "/dev"))
(allow mach-lookup
  (global-name "com.apple.system.opendirectoryd.libinfo")
  (global-name "com.apple.system.notification_center")
  (global-name "com.apple.system.logger")
  (global-name "com.apple.logd")
  (global-name "com.apple.SecurityServer")
  (global-name "com.apple.trustd.agent"))
(allow file-read-metadata)
(allow file-read*
  (literal "/")
  (subpath "/nix/store")
  (subpath "/nix/var/nix/daemon-socket")
  (subpath "/bin")
  (subpath "/usr/bin")
  (subpath "/usr/lib")
  (subpath "/usr/share")
  (subpath "/System")
  (subpath "/Library/Apple")
  (subpath "/private/etc")
  (subpath "/private/var/db/timezone")
  (subpath "/dev"))
(deny file-read* (subpath "/private/etc/nix"))
(allow file-write*
  (literal "/dev/null")
  (literal "/dev/zero")
  (literal "/dev/tty")
  (regex #"^/dev/fd/"))
"#;

const NETWORK_PROFILE: &str = r#"(allow system-socket)
(allow network-outbound)
(deny network-outbound (remote ip "localhost:*"))
(deny network-outbound (remote unix-socket))
(allow mach-lookup
  (global-name "com.apple.dnssd.service")
  (global-name "com.apple.SystemConfiguration.configd"))
"#;

pub struct Seatbelt {
    tmp: tempfile::TempDir,
    path: String,
}

impl Seatbelt {
    pub fn new() -> Result<Self> {
        if !Path::new(SANDBOX_EXEC).exists() {
            anyhow::bail!("{SANDBOX_EXEC} not found; use --no-sandbox to run unsandboxed");
        }
        let tmp = tempfile::tempdir().context("failed to create sandbox temp dir")?;
        std::fs::create_dir(tmp.path().join("home")).context("failed to create sandbox home")?;
        let nix_dir = find_in_path("nix")
            .context("`nix` not found in PATH")?
            .canonicalize()
            .context("failed to resolve `nix`")?;
        let nix_dir = nix_dir.parent().context("`nix` has no parent directory")?;
        let path = format!("{}:/usr/bin:/bin", nix_dir.display());
        Ok(Self { tmp, path })
    }

    pub fn command(
        &self,
        checkout: &Checkout,
        network: bool,
        program: &str,
        args: &[OsString],
    ) -> Command {
        let tmp = canonical(self.tmp.path());
        let profile = profile(checkout, &tmp, network);
        let mut cmd = Command::new(SANDBOX_EXEC);
        cmd.arg("-p").arg(profile).arg(program).args(args);
        cmd.env_clear()
            .env("PATH", &self.path)
            .env("HOME", tmp.join("home"))
            .env("TMPDIR", &tmp)
            .env("NIX_REMOTE", "daemon");
        if let Some(certs) = std::env::var_os("NIX_SSL_CERT_FILE") {
            cmd.env("NIX_SSL_CERT_FILE", certs);
        }
        cmd
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn profile(checkout: &Checkout, tmp: &Path, network: bool) -> String {
    let repo = canonical(checkout.root());
    let mut out = String::from(BASE_PROFILE);
    let (repo_s, tmp_s) = (quote(&repo), quote(tmp));
    out.push_str(&format!(
        "(allow file-read* (subpath {repo_s}) (subpath {tmp_s}))\n"
    ));
    out.push_str(&format!(
        "(allow file-write* (subpath {repo_s}) (subpath {tmp_s}))\n"
    ));
    for git in git_dirs(&repo, checkout) {
        out.push_str(&format!("(allow file-read* (subpath {}))\n", quote(&git)));
        out.push_str(&format!("(deny file-write* (subpath {}))\n", quote(&git)));
    }
    if network {
        out.push_str(NETWORK_PROFILE);
    }
    out.push_str(&format!(
        "(allow network-outbound (remote unix-socket (path-literal {})))\n",
        quote(Path::new(NIX_DAEMON_SOCKET))
    ));
    out
}

// `.git` stays unwritable even when absent, so a check cannot create one.
fn git_dirs(repo: &Path, checkout: &Checkout) -> Vec<PathBuf> {
    let mut out = vec![repo.join(".git")];
    for p in checkout.git_paths().iter().map(|p| canonical(p)) {
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

fn quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_scopes_writes_and_network() {
        let repo = Checkout::resolve("/Users/me/src/x");
        let p = profile(&repo, Path::new("/private/tmp/t"), false);
        assert!(p.starts_with("(version 1)\n(deny default)"));
        assert!(p.contains("(allow file-write* (subpath \"/Users/me/src/x\")"));
        assert!(p.contains("(deny file-write* (subpath \"/Users/me/src/x/.git\"))"));
        assert!(!p.contains("(allow network-outbound)\n"));
        assert!(p.contains("daemon-socket/socket"));
        assert!(p.contains("(deny file-read* (subpath \"/private/etc/nix\"))"));
        let p = profile(&repo, Path::new("/private/tmp/t"), true);
        assert!(p.contains("(deny network-outbound (remote ip \"localhost:*\"))"));
        let deny_unix = p
            .find("(deny network-outbound (remote unix-socket))")
            .unwrap();
        assert!(deny_unix < p.find("daemon-socket/socket\")))").unwrap());
    }

    #[test]
    fn quotes_paths() {
        assert_eq!(quote(Path::new("/a\"b\\c")), "\"/a\\\"b\\\\c\"");
    }

    #[tokio::test]
    #[ignore = "needs macOS sandbox-exec"]
    async fn hides_home_and_blocks_writes_outside_checkout() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        let home = std::env::var("HOME").unwrap();
        let sb = Seatbelt::new().unwrap();
        let script = format!(
            "ls {home} >/dev/null 2>&1 && echo saw-home; echo ok > out && echo wrote-repo; echo x \
             > .git/x 2>/dev/null && echo wrote-git; echo x > {home}/.ekaci-probe 2>/dev/null && \
             echo wrote-home; echo done"
        );
        let out = sb
            .command(
                &Checkout::resolve(repo.path()),
                false,
                "/bin/sh",
                &["-c".into(), script.into()],
            )
            .current_dir(repo.path())
            .output()
            .await
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("wrote-repo") && stdout.contains("done"),
            "{stdout}"
        );
        for leak in ["saw-home", "wrote-git", "wrote-home"] {
            assert!(!stdout.contains(leak), "{leak}: {stdout}");
        }
    }
}
