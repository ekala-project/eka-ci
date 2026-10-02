use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::{
    DEFAULT_MEMORY_LIMIT_MB, DEFAULT_TIMEOUT, Network, Sandbox, SandboxSpec, SandboxedChild,
    resolve_program,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalSandboxConfig {
    pub timeout: Duration,
    pub memory_limit_mb: u64,
    pub allowed_uris: Vec<String>,
}

impl Default for EvalSandboxConfig {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_TIMEOUT,
            memory_limit_mb: DEFAULT_MEMORY_LIMIT_MB,
            allowed_uris: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct EvalSandbox {
    sandbox: Arc<Sandbox>,
    config: EvalSandboxConfig,
}

impl EvalSandbox {
    pub fn new(sandbox: Arc<Sandbox>, config: EvalSandboxConfig) -> Self {
        Self { sandbox, config }
    }

    pub async fn spawn_nix_eval_jobs(
        &self,
        file: &Path,
        extra_roots: &[PathBuf],
    ) -> Result<SandboxedChild> {
        let program = resolve_program("nix-eval-jobs")?;
        let spec = self.nix_eval_jobs_spec(program, file, extra_roots);
        self.sandbox
            .spawn(&spec)
            .await
            .context("failed to start sandboxed nix-eval-jobs")
    }

    fn nix_eval_jobs_spec(
        &self,
        program: PathBuf,
        file: &Path,
        extra_roots: &[PathBuf],
    ) -> SandboxSpec {
        let mut roots = vec![readable_root(file)];
        for root in extra_roots {
            if !roots.contains(root) {
                roots.push(root.clone());
            }
        }
        let mut spec = SandboxSpec::new(program)
            .args(nix_eval_jobs_args(&self.config, file, &roots))
            .network(Network::Filtered)
            .timeout(self.config.timeout)
            .memory_limit_bytes(self.config.memory_limit_mb.saturating_mul(1024 * 1024));
        if let Some(dir) = roots.iter().find(|r| r.is_dir()) {
            spec = spec.cwd(dir);
        }
        roots.into_iter().fold(spec, SandboxSpec::ro_path)
    }
}

pub fn nix_eval_jobs_args(
    cfg: &EvalSandboxConfig,
    file: &Path,
    roots: &[PathBuf],
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "--option".into(),
        "restrict-eval".into(),
        "true".into(),
        "--option".into(),
        "allowed-uris".into(),
        cfg.allowed_uris.join(" ").into(),
        // restrict-eval does not cover builtins.getFlake, which could fetch any URL.
        "--option".into(),
        "experimental-features".into(),
        "nix-command".into(),
        // Recycle workers before RLIMIT_AS turns an allocation failure into a crash.
        "--max-memory-size".into(),
        (cfg.memory_limit_mb / 2).max(1).to_string().into(),
    ];
    // Named entries keep `<nixpkgs>`-style lookups from resolving into the roots.
    for (i, root) in roots.iter().enumerate() {
        let mut entry = OsString::from(format!("ekaci-root-{i}="));
        entry.push(root);
        args.extend(["-I".into(), entry]);
    }
    args.extend(["--show-input-drvs".into(), "--meta".into(), file.into()]);
    args
}

pub fn readable_root(file: &Path) -> PathBuf {
    file.ancestors()
        .skip(1)
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| file.to_path_buf())
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

    #[test]
    fn args_enable_restrict_eval_and_allowed_uris() {
        let cfg = EvalSandboxConfig {
            allowed_uris: vec!["https://a/".into(), "github:".into()],
            memory_limit_mb: 4096,
            ..Default::default()
        };
        let roots = [PathBuf::from("/w")];
        let s = joined(&nix_eval_jobs_args(&cfg, Path::new("/w/f.nix"), &roots));
        assert!(
            s.starts_with("--option restrict-eval true --option allowed-uris https://a/ github:")
        );
        assert!(s.contains("--option experimental-features nix-command -"));
        assert!(s.contains("--max-memory-size 2048"));
        assert!(s.contains("-I ekaci-root-0=/w"));
        assert!(s.ends_with("--show-input-drvs --meta /w/f.nix"));
    }

    #[test]
    fn duplicate_roots_are_listed_once() {
        let sb = EvalSandbox::new(
            Arc::new(Sandbox::with_paths("bwrap".into(), "helper".into())),
            EvalSandboxConfig::default(),
        );
        let extra = [
            PathBuf::from("/x"),
            PathBuf::from("/w/f.nix"),
            PathBuf::from("/x"),
        ];
        let spec = sb.nix_eval_jobs_spec("/bin/nej".into(), Path::new("/w/f.nix"), &extra);
        assert_eq!(
            spec.ro_paths,
            vec![PathBuf::from("/w/f.nix"), PathBuf::from("/x")]
        );
    }

    #[test]
    fn readable_root_is_worktree_or_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sub/default.nix");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        assert_eq!(readable_root(&file), file);
        std::fs::write(dir.path().join(".git"), "gitdir: elsewhere").unwrap();
        assert_eq!(readable_root(&file), dir.path());
    }
}
