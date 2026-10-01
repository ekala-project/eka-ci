use std::path::Path;
use std::process::Output;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tracing::debug;

use super::GitRepo;

/// Timeout for git clone operations (large repos may take a while).
const GIT_CLONE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Timeout for git fetch operations.
const GIT_FETCH_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Timeout for git worktree operations (quick local operation).
const GIT_WORKTREE_TIMEOUT: Duration = Duration::from_secs(60);
const GIT_REV_PARSE_TIMEOUT: Duration = Duration::from_secs(30);

const ORIGIN: &str = "origin";
const ORIGIN_BRANCHES_REFSPEC: &str = "+refs/heads/*:refs/remotes/origin/*";

async fn run_git<I, S>(dir: Option<&Path>, args: I, timeout: Duration, what: &str) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let mut cmd = Command::new("git");
    if let Some(dir) = dir {
        cmd.current_dir(dir);
    }
    tokio::time::timeout(timeout, cmd.args(args).output())
        .await
        .with_context(|| format!("git {what} timed out"))?
        .with_context(|| format!("failed to execute git {what}"))
}

fn clone_args<'a>(git_url: &'a str, path: &'a str) -> [&'a str; 4] {
    ["clone", "--", git_url, path]
}

// `--` ends option parsing so a PR-controlled branch name is never read as a flag.
fn fetch_args<'a>(remote: &'a str, reference: &'a str) -> [&'a str; 4] {
    ["fetch", "--", remote, reference]
}

fn worktree_add_args<'a>(worktree_dir: &'a str, commitish: &'a str) -> [&'a str; 6] {
    ["worktree", "add", "--detach", "--", worktree_dir, commitish]
}

fn rev_parse_args(rev: &str) -> Vec<String> {
    vec![
        "rev-parse".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
        "--end-of-options".to_string(),
        format!("{rev}^{{commit}}"),
    ]
}

pub async fn clone_git_repo(git_url: &str, path: &str) -> Result<Output> {
    debug!("Attempting to checkout {} at {}", git_url, path);

    run_git(None, clone_args(git_url, path), GIT_CLONE_TIMEOUT, "clone").await
}

async fn run_fetch(repo_dir: &Path, remote: &str, reference: &str) -> Result<()> {
    let args = fetch_args(remote, reference);
    let out = run_git(Some(repo_dir), args, GIT_FETCH_TIMEOUT, "fetch").await?;

    if !out.status.success() {
        bail!(
            "git fetch of {} from {} failed: {}",
            reference,
            remote,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    Ok(())
}

pub async fn fetch_remote_repo<P: AsRef<Path>>(
    repo_dir: P,
    repo: &GitRepo,
    reference: &str,
) -> Result<()> {
    debug!("fetching branch {} from {}", reference, repo.checkout_url());
    run_fetch(repo_dir.as_ref(), &repo.checkout_url(), reference).await
}

pub(crate) async fn rev_exists(repo_dir: &Path, rev: &str) -> Result<bool> {
    let args = rev_parse_args(rev);
    let out = run_git(Some(repo_dir), args, GIT_REV_PARSE_TIMEOUT, "rev-parse").await?;
    Ok(out.status.success())
}

pub(crate) async fn ensure_rev_fetched(repo_dir: &Path, rev: &str) -> Result<()> {
    if rev_exists(repo_dir, rev).await? {
        return Ok(());
    }
    debug!(
        "{} not found in {}, fetching origin",
        rev,
        repo_dir.display()
    );
    run_fetch(repo_dir, ORIGIN, ORIGIN_BRANCHES_REFSPEC).await?;
    if rev_exists(repo_dir, rev).await? {
        return Ok(());
    }
    // Only a full object id is safe as a refspec; anything else could carry `src:dst`.
    if !is_object_id(rev) {
        bail!("{} not found in origin's branches", rev);
    }
    debug!(
        "{} still missing after branch fetch, fetching it directly",
        rev
    );
    run_fetch(repo_dir, ORIGIN, rev).await
}

fn is_object_id(rev: &str) -> bool {
    matches!(rev.len(), 40 | 64) && rev.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub async fn add_git_worktree<P: AsRef<Path>>(
    repo_dir: P,
    worktree_dir: &str,
    commitish: &str,
) -> Result<Output> {
    debug!(
        "Creating worktree at {} on commit {}",
        worktree_dir, commitish
    );

    let args = worktree_add_args(worktree_dir, commitish);
    run_git(
        Some(repo_dir.as_ref()),
        args,
        GIT_WORKTREE_TIMEOUT,
        "worktree add",
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::Command as StdCommand;

    use tempfile::TempDir;

    use super::*;
    use crate::{GitProtocol, GitWorkspace};

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=ekaci", "-c", "user.email=ekaci@test"])
            .args([
                "-c",
                "init.defaultBranch=main",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("failed to run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit(repo: &Path, msg: &str) -> String {
        git(repo, &["commit", "--allow-empty", "-q", "-m", msg]);
        git(repo, &["rev-parse", "HEAD"])
    }

    struct Fixture {
        tmp: TempDir,
        remote: PathBuf,
        repos_root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let tmp = TempDir::new().unwrap();
            let remote = tmp.path().join("remote");
            std::fs::create_dir_all(&remote).unwrap();
            git(&remote, &["init", "-q"]);
            commit(&remote, "initial");

            let repos_root = tmp.path().join("repos");
            let fx = Self {
                tmp,
                remote,
                repos_root,
            };
            let master = fx.workspace("unused").master_path();
            std::fs::create_dir_all(master.parent().unwrap()).unwrap();
            let remote_str = fx.remote.to_str().unwrap();
            git(
                fx.tmp.path(),
                &["clone", "-q", remote_str, master.to_str().unwrap()],
            );
            fx
        }

        fn workspace(&self, rev: &str) -> GitWorkspace {
            let repo = GitRepo {
                protocol: GitProtocol::Https,
                domain: "forge.invalid".to_string(),
                owner: "owner".to_string(),
                repo: "repo".to_string(),
            };
            GitWorkspace::new(repo, rev, self.repos_root.clone())
        }
    }

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
            .block_on(fut)
    }

    fn head_of(worktree: &Path) -> String {
        git(worktree, &["rev-parse", "HEAD"])
    }

    #[test]
    fn existing_clone_fetches_new_branch_commit() {
        let fx = Fixture::new();
        let new_sha = commit(&fx.remote, "pushed after first clone");

        let ws = fx.workspace(&new_sha);
        block_on(ws.create_worktree()).expect("checkout of a newly pushed SHA must fetch first");
        assert_eq!(head_of(&ws.worktree_path()), new_sha);
    }

    #[test]
    fn existing_clone_fetches_commit_not_on_any_branch() {
        let fx = Fixture::new();
        git(
            &fx.remote,
            &["config", "uploadpack.allowAnySHA1InWant", "true"],
        );
        git(&fx.remote, &["checkout", "-q", "-b", "scratch"]);
        let orphan_sha = commit(&fx.remote, "only reachable by sha");
        git(&fx.remote, &["checkout", "-q", "main"]);
        git(&fx.remote, &["branch", "-q", "-D", "scratch"]);

        let ws = fx.workspace(&orphan_sha);
        block_on(ws.create_worktree())
            .expect("direct SHA fetch fallback must make the commit available");
        assert_eq!(head_of(&ws.worktree_path()), orphan_sha);
    }

    #[test]
    fn missing_commit_reports_error() {
        let fx = Fixture::new();
        let ws = fx.workspace("0123456789abcdef0123456789abcdef01234567");
        let err = block_on(ws.create_worktree()).unwrap_err();
        assert!(err.to_string().contains("git fetch"), "{err:#}");
        assert!(!ws.worktree_path().exists());
    }

    #[test]
    fn fetch_treats_dash_prefixed_reference_as_refspec() {
        let fx = Fixture::new();
        let master = fx.workspace("unused").master_path();
        let err = block_on(run_fetch(&master, "origin", "--upload-pack=touch pwned")).unwrap_err();
        assert!(err.to_string().contains("git fetch"), "{err:#}");
        assert!(!master.join("pwned").exists());
    }

    #[test]
    fn missing_non_object_id_is_not_fetched_as_refspec() {
        let fx = Fixture::new();
        let rev = "+refs/heads/master:refs/heads/hijacked";
        let ws = fx.workspace(rev);
        let err = block_on(ws.create_worktree()).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err:#}");
        let master = ws.master_path();
        assert!(!block_on(rev_exists(&master, "refs/heads/hijacked")).unwrap());
    }

    #[test]
    fn object_id_detection() {
        assert!(is_object_id(&"a".repeat(40)));
        assert!(is_object_id(&"0".repeat(64)));
        assert!(!is_object_id(&"A".repeat(40)));
        assert!(!is_object_id("master"));
        assert!(!is_object_id(&"a".repeat(41)));
    }

    #[test]
    fn args_end_option_parsing_before_user_input() {
        assert_eq!(fetch_args("url", "-x"), ["fetch", "--", "url", "-x"]);
        assert_eq!(clone_args("-u", "p"), ["clone", "--", "-u", "p"]);
        assert_eq!(
            worktree_add_args("d", "-x"),
            ["worktree", "add", "--detach", "--", "d", "-x"]
        );
        assert_eq!(rev_parse_args("-x")[3], "--end-of-options");
    }
}
