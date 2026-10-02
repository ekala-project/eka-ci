#![cfg(target_os = "linux")]

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use sandbox::{Sandbox, SandboxExit, SandboxSpec, find_in_path, resolve_program};
use tokio::io::AsyncReadExt;

fn helper() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ekaci-sandbox-helper"))
}

async fn sandboxed_sh(spec: SandboxSpec) -> (String, SandboxExit) {
    let sb = Sandbox::with_paths(find_in_path("bwrap").unwrap(), helper());
    let mut child = sb.spawn(&spec).await.unwrap();
    let mut out = String::new();
    let mut stdout = child.stdout.take().unwrap();
    stdout.read_to_string(&mut out).await.unwrap();
    (out, child.wait().await.unwrap())
}

#[tokio::test]
#[ignore = "needs bwrap and user namespaces"]
async fn bwrap_hides_host_env_and_files() {
    // SAFETY: set before the sandbox is spawned; nothing else reads it.
    unsafe { std::env::set_var("EKACI_PRIMITIVE_SECRET", "leak") };
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret");
    std::fs::write(&secret, "leak\n").unwrap();
    let work = tempfile::tempdir().unwrap();
    let script = format!(
        "echo \"env=[$EKACI_PRIMITIVE_SECRET] home=$HOME\"; read -r l < {secret} && echo \
         \"read=$l\"; echo x > {work}/f 2>/dev/null && echo wrote-ro",
        secret = secret.display(),
        work = work.path().display(),
    );
    let spec = SandboxSpec::new(resolve_program("sh").unwrap())
        .args(["-c", &script])
        .ro_path(work.path());
    let (out, exit) = sandboxed_sh(spec).await;
    assert!(out.contains("env=[] home=/tmp/home"), "{out}");
    assert!(!out.contains("leak") && !out.contains("wrote-ro"), "{out}");
    assert!(matches!(exit, SandboxExit::Exited(_)));
}

#[tokio::test]
#[ignore = "needs bwrap and user namespaces"]
async fn sandbox_timeout_kills_whole_tree() {
    let spec = SandboxSpec::new(resolve_program("sh").unwrap())
        .args(["-c", "while :; do :; done & while :; do :; done"])
        .timeout(Duration::from_millis(500));
    let (_, exit) = sandboxed_sh(spec).await;
    assert_eq!(exit, SandboxExit::TimedOut(Duration::from_millis(500)));
}

#[test]
#[ignore = "needs a landlock-enabled kernel"]
fn landlock_limits_filesystem_to_listed_paths() {
    let allowed = tempfile::tempdir().unwrap();
    std::fs::write(allowed.path().join("ok"), "fine\n").unwrap();
    let denied = tempfile::tempdir().unwrap();
    std::fs::write(denied.path().join("secret"), "leak\n").unwrap();
    let script = format!(
        "read -r a < {ok} && echo \"a=$a\"; read -r b < {secret} && echo \"b=$b\"; echo x > \
         {ok}.new 2>/dev/null && echo wrote",
        ok = allowed.path().join("ok").display(),
        secret = denied.path().join("secret").display(),
    );
    let out = Command::new(helper())
        .args(["--ro", "/nix/store", "--ro"])
        .arg(allowed.path())
        .arg("--")
        .arg(resolve_program("sh").unwrap())
        .args(["-c", &script])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout,
        "a=fine\n",
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
