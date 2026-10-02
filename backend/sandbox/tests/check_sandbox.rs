#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sandbox::check::{CheckSandbox, Limits};
use sandbox::checkout::Checkout;
use sandbox::devshell::DevShell;
use sandbox::{Sandbox, SandboxExit, find_in_path};

const SECRET: &str = "ekaci-check-secret";

fn check_sandbox(timeout: Duration) -> CheckSandbox {
    let helper = PathBuf::from(env!("CARGO_BIN_EXE_ekaci-sandbox-helper"));
    let sandbox = Sandbox::with_paths(find_in_path("bwrap").expect("bwrap"), helper)
        .with_pasta(find_in_path("pasta").expect("pasta"));
    let limits = Limits {
        timeout,
        memory_limit_mb: 2048,
    };
    CheckSandbox::new(Arc::new(sandbox), limits).with_dev_shell_limits(limits)
}

fn at(dir: &Path) -> Checkout {
    Checkout::resolve(dir)
}

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
    // SAFETY: set before any sandbox is spawned; nothing else reads it.
    unsafe { std::env::set_var("EKACI_CHECK_SECRET", SECRET) };
    let checkout = tempfile::tempdir().unwrap();
    std::fs::create_dir(checkout.path().join(".git")).unwrap();
    std::fs::write(checkout.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret");
    std::fs::write(&secret, format!("{SECRET}\n")).unwrap();
    (checkout, outside, secret)
}

fn script(secret: &Path) -> String {
    format!(
        "echo \"env=[$EKACI_CHECK_SECRET] home=$HOME given=$GIVEN\"; read -r l < {secret} \
         2>/dev/null && echo \"$l\"; for f in /home/*/* /root/*; do [ -e \"$f\" ] && echo \
         saw-home; done; echo built > result.txt && echo wrote-checkout; echo x > .git/HEAD \
         2>/dev/null && echo wrote-git; (exec 3<>/dev/tcp/192.168.1.1/80) 2>/dev/null && echo \
         lan-open; (exec 3<>/dev/tcp/example.com/443) 2>/dev/null && echo internet-open; true",
        secret = secret.display()
    )
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, nix and user namespaces"]
async fn check_sees_only_checkout_and_given_env() {
    let (checkout, _outside, secret) = fixture();
    let env = vec![("GIVEN".to_string(), "yes".to_string())];
    let out = check_sandbox(Duration::from_secs(60))
        .run(&at(checkout.path()), &script(&secret), false, &env)
        .await
        .unwrap();
    assert!(out.success(), "{out:?}");
    assert!(
        out.stdout.contains("env=[] home=/tmp/home given=yes"),
        "{out:?}"
    );
    assert!(out.stdout.contains("wrote-checkout"), "{out:?}");
    for leak in [SECRET, "saw-home", "wrote-git", "lan-open", "internet-open"] {
        assert!(!out.stdout.contains(leak), "{leak}: {out:?}");
    }
    let head = std::fs::read_to_string(checkout.path().join(".git/HEAD")).unwrap();
    assert_eq!(head, "ref: refs/heads/main\n");
    assert!(checkout.path().join("result.txt").exists());
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, nix, user namespaces and internet access"]
async fn check_with_network_reaches_internet_only() {
    let (checkout, _outside, secret) = fixture();
    let out = check_sandbox(Duration::from_secs(60))
        .run(&at(checkout.path()), &script(&secret), true, &[])
        .await
        .unwrap();
    assert!(out.stdout.contains("internet-open"), "{out:?}");
    assert!(!out.stdout.contains("lan-open"), "{out:?}");
    assert!(!out.stdout.contains(SECRET), "{out:?}");
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, nix and user namespaces"]
async fn check_timeout_kills_the_tree() {
    let (checkout, _outside, _) = fixture();
    let started = Instant::now();
    let out = check_sandbox(Duration::from_secs(2))
        .run(
            &at(checkout.path()),
            "(while :; do :; done) & while :; do :; done",
            false,
            &[],
        )
        .await
        .unwrap();
    assert_eq!(out.exit, SandboxExit::TimedOut(Duration::from_secs(2)));
    assert_eq!(out.exit_code(), -1);
    assert!(started.elapsed() < Duration::from_secs(20));
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, nix and user namespaces"]
async fn planted_git_does_not_outlive_the_check() {
    let (_, _outside, secret) = fixture();
    let checkout = tempfile::tempdir().unwrap();
    let dot_git = checkout.path().join(".git");
    let sb = check_sandbox(Duration::from_secs(60));
    let plant = format!(
        "echo 'gitdir: {}' > .git && echo planted",
        secret.parent().unwrap().display()
    );
    let out = sb
        .run(&at(checkout.path()), &plant, false, &[])
        .await
        .unwrap();
    assert!(out.stdout.contains("planted"), "{out:?}");
    assert!(
        dot_git.symlink_metadata().is_err(),
        "planted .git left behind"
    );
    std::fs::write(
        &dot_git,
        format!("gitdir: {}\n", secret.parent().unwrap().display()),
    )
    .unwrap();
    let probe = format!("read -r l < {} && echo \"$l\"; echo done", secret.display());
    let out = sb
        .run(&at(checkout.path()), &probe, false, &[])
        .await
        .unwrap();
    assert!(out.stdout.contains("done"), "{out:?}");
    assert!(!out.stdout.contains(SECRET), "{out:?}");
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, nix and user namespaces"]
async fn dev_shell_eval_cannot_see_host() {
    let (checkout, _outside, secret) = fixture();
    let shell_nix = format!(
        "throw \"probe env=[${{builtins.getEnv \"EKACI_CHECK_SECRET\"}}] file=${{if \
         builtins.pathExists {secret} then \"yes\" else \"no\"}}\"",
        secret = secret.display()
    );
    std::fs::write(checkout.path().join("shell.nix"), shell_nix).unwrap();
    let err = check_sandbox(Duration::from_secs(120))
        .dev_shell_env(&at(checkout.path()), DevShell::ShellNix(None))
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("probe env=[] file=no"), "{msg}");
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, nix and user namespaces"]
async fn dev_shell_requires_entry_file() {
    let (checkout, _outside, _) = fixture();
    let err = check_sandbox(Duration::from_secs(30))
        .dev_shell_env(&at(checkout.path()), DevShell::Flake(None))
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("no flake.nix"), "{err:#}");
}

fn flake_checkout(shell_attrs: &str) -> tempfile::TempDir {
    let nixpkgs = std::env::var("EKACI_TEST_NIXPKGS").expect("EKACI_TEST_NIXPKGS");
    let checkout = tempfile::tempdir().unwrap();
    let flake = format!(
        "{{ inputs.nixpkgs.url = \"path:{nixpkgs}\"; outputs = {{ nixpkgs, ... }}: let p = \
         nixpkgs.legacyPackages.x86_64-linux; in {{ devShells.x86_64-linux.ci = p.mkShellNoCC {{ \
         packages = [ p.hello ]; {shell_attrs} }}; }}; }}"
    );
    std::fs::write(checkout.path().join("flake.nix"), flake).unwrap();
    git(checkout.path(), &["init", "-q"]);
    git(checkout.path(), &["add", "flake.nix"]);
    checkout
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, nix, user namespaces and EKACI_TEST_NIXPKGS"]
async fn flake_dev_shell_provides_check_environment() {
    let checkout =
        flake_checkout("EKACI_MARK = \"from-shell\"; shellHook = \"echo Welcome; echo hi >&2\";");
    let sb = check_sandbox(Duration::from_secs(600));
    let env = sb
        .dev_shell_env(&at(checkout.path()), DevShell::Flake(Some("ci")))
        .await
        .unwrap();
    assert!(
        env.iter()
            .any(|(k, v)| k == "EKACI_MARK" && v == "from-shell")
    );
    assert!(
        env.iter()
            .all(|(k, _)| !k.contains('\n') && k != "TMPDIR" && k != "HOME")
    );
    let out = sb
        .run(
            &at(checkout.path()),
            "hello && echo \"$EKACI_MARK\"",
            false,
            &env,
        )
        .await
        .unwrap();
    assert!(out.success(), "{out:?}");
    assert!(out.stdout.contains("Hello, world!\nfrom-shell"), "{out:?}");
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, nix, user namespaces and EKACI_TEST_NIXPKGS"]
async fn flake_shell_hook_cannot_see_host() {
    let (_, _outside, secret) = fixture();
    let hook = format!(
        "shellHook = \"echo probe env=[$EKACI_CHECK_SECRET] file=[$(cat {} 2>&1)] >&2; exit 3\";",
        secret.display()
    );
    let checkout = flake_checkout(&hook);
    let err = check_sandbox(Duration::from_secs(600))
        .dev_shell_env(&at(checkout.path()), DevShell::Flake(Some("ci")))
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("probe env=[] file=[cat:"), "{msg}");
    assert!(!msg.contains(SECRET), "{msg}");
}

#[tokio::test]
#[ignore = "needs bwrap, pasta, nix, user namespaces and EKACI_TEST_NIXPKGS"]
async fn flake_shell_capture_times_out() {
    let checkout = flake_checkout("shellHook = \"sleep 600 & sleep 600\";");
    check_sandbox(Duration::from_secs(600))
        .dev_shell_env(&at(flake_checkout("").path()), DevShell::Flake(Some("ci")))
        .await
        .unwrap();
    let started = Instant::now();
    let err = check_sandbox(Duration::from_secs(15))
        .dev_shell_env(&at(checkout.path()), DevShell::Flake(Some("ci")))
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("timed out"), "{err:#}");
    assert!(started.elapsed() < Duration::from_secs(45));
}

fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}
