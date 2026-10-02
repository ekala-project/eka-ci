#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sandbox::eval::{EvalSandbox, EvalSandboxConfig};
use sandbox::{Sandbox, SandboxExit, find_in_path};
use serde_json::Value;
use tokio::io::AsyncReadExt;

const SECRET: &str = "ekaci-test-secret-value";

fn eval_sandbox(config: EvalSandboxConfig) -> EvalSandbox {
    let bwrap = find_in_path("bwrap").expect("bwrap in PATH");
    let helper = PathBuf::from(env!("CARGO_BIN_EXE_ekaci-sandbox-helper"));
    let pasta = find_in_path("pasta").expect("pasta in PATH");
    let sandbox = Sandbox::with_paths(bwrap, helper).with_pasta(pasta);
    EvalSandbox::new(Arc::new(sandbox), config)
}

fn worktree(expr: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".git"), "gitdir: /nonexistent").unwrap();
    std::fs::write(dir.path().join("other.nix"), "\"ok\"").unwrap();
    let file = dir.path().join("default.nix");
    std::fs::write(&file, expr).unwrap();
    (dir, file)
}

struct EvalRun {
    lines: Vec<Value>,
    stderr: String,
    exit: SandboxExit,
}

async fn run(sb: &EvalSandbox, file: &Path) -> EvalRun {
    let mut child = sb.spawn_nix_eval_jobs(file, &[]).await.unwrap();
    let (mut out, mut err) = (String::new(), String::new());
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let (r1, r2) = tokio::join!(
        stdout.read_to_string(&mut out),
        stderr.read_to_string(&mut err)
    );
    r1.unwrap();
    r2.unwrap();
    let exit = child.wait().await.unwrap();
    let lines = out
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    EvalRun {
        lines,
        stderr: err,
        exit,
    }
}

fn entry<'a>(run: &'a EvalRun, attr: &str) -> &'a Value {
    run.lines
        .iter()
        .find(|l| l["attr"] == attr)
        .unwrap_or_else(|| panic!("no output for `{attr}`: {:?}", run.lines))
}

fn escape_expr(secret_file: &Path) -> String {
    format!(
        r#"let
  mk = name: derivation {{ inherit name; builder = "/bin/sh"; system = builtins.currentSystem; }};
in {{
  legit = mk "legit-${{import ./other.nix}}";
  env = mk "env-${{builtins.getEnv "EKACI_TEST_SECRET"}}";
  readEtc = mk (builtins.hashString "sha256" (builtins.readFile /etc/passwd));
  readSecret = mk (builtins.readFile {secret});
  fetch = mk (builtins.hashFile "sha256" (builtins.fetchurl "https://example.com/"));
  getFlakeUrl = mk (builtins.hashString "sha256" (builtins.getFlake "https://example.com/leak.tar.gz").outPath);
  getFlakeGh = mk (builtins.hashString "sha256" (builtins.getFlake "github:NixOS/patchelf/0.18.0").outPath);
  fetchTree = mk (builtins.hashString "sha256" (builtins.fetchTree "https://example.com/leak.tar.gz").outPath);
}}
"#,
        secret = secret_file.display()
    )
}

#[tokio::test]
#[ignore = "needs nix daemon, nix-eval-jobs and bwrap"]
async fn untrusted_expression_cannot_escape() {
    // SAFETY: set before any other thread of this test reads the env.
    unsafe { std::env::set_var("EKACI_TEST_SECRET", SECRET) };
    let outside = tempfile::tempdir().unwrap();
    let secret_file = outside.path().join("secret");
    std::fs::write(&secret_file, SECRET).unwrap();
    let (_wt, file) = worktree(&escape_expr(&secret_file));

    let run = run(&eval_sandbox(EvalSandboxConfig::default()), &file).await;

    assert!(
        entry(&run, "legit")["drvPath"]
            .as_str()
            .unwrap()
            .ends_with("-legit-ok.drv")
    );
    assert!(
        entry(&run, "env")["drvPath"]
            .as_str()
            .unwrap()
            .ends_with("-env-.drv")
    );
    for (attr, reason) in [
        ("readEtc", "forbidden in restricted mode"),
        ("readSecret", "forbidden in restricted mode"),
        ("fetch", "forbidden in restricted mode"),
        ("getFlakeUrl", "'flakes' is disabled"),
        ("getFlakeGh", "'flakes' is disabled"),
        ("fetchTree", "attribute 'fetchTree' missing"),
    ] {
        let error = entry(&run, attr)["error"].as_str().unwrap_or_default();
        assert!(
            error.contains(reason),
            "`{attr}` must fail with `{reason}`: {error:?}"
        );
    }
    let all_output = format!("{:?}{}", run.lines, run.stderr);
    assert!(!all_output.contains(SECRET), "secret leaked: {all_output}");
    assert!(matches!(run.exit, SandboxExit::Exited(_)));
}

#[tokio::test]
#[ignore = "needs nix daemon, nix-eval-jobs and bwrap"]
async fn allowed_uris_admits_listed_prefix() {
    let expr = r#"{ fetch = derivation { name = "f"; builder = "/bin/sh"; system = builtins.currentSystem;
        h = builtins.hashFile "sha256" (builtins.fetchurl "https://example.com/"); }; }"#;
    let (_wt, file) = worktree(expr);
    let config = EvalSandboxConfig {
        allowed_uris: vec!["https://example.com/".into()],
        ..Default::default()
    };
    let run = run(&eval_sandbox(config), &file).await;
    assert!(
        entry(&run, "fetch")["drvPath"].is_string(),
        "{:?} {}",
        run.lines,
        run.stderr
    );
}

#[tokio::test]
#[ignore = "needs nix daemon, nix-eval-jobs and bwrap"]
async fn runaway_eval_is_killed_on_timeout() {
    let expr = r#"let
  inner = builtins.foldl' (b: j: b + j) 0 (builtins.genList (x: x) 1000);
in { spin = builtins.foldl' (a: i: a + inner) 0 (builtins.genList (x: x) 100000000); }
"#;
    let (_wt, file) = worktree(expr);
    let config = EvalSandboxConfig {
        timeout: Duration::from_secs(3),
        ..Default::default()
    };
    let started = Instant::now();
    let run = run(&eval_sandbox(config), &file).await;
    assert_eq!(run.exit, SandboxExit::TimedOut(Duration::from_secs(3)));
    assert!(started.elapsed() < Duration::from_secs(30));
}
