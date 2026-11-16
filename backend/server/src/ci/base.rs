//! The base-commit checkout a PR head carries so its evals can evaluate
//! the base first (see `nix::base_eval`).

use std::path::{Path, PathBuf};

use tracing::info;

use super::{RepoReader, resolve_file_path};
use crate::db::github::{delete_jobset, stale_head_jobset};
use crate::github::CICheckInfo;
use crate::nix::BaseEvalJob;

/// A PR base commit's worktree and check info.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BaseCheckout {
    pub repo_path: PathBuf,
    pub ci_info: CICheckInfo,
}

impl BaseCheckout {
    /// The base-commit eval of the job whose config file is `job_file`, if
    /// the base checkout has that file.
    pub(super) fn eval_job_for(&self, job_file: &Path) -> Option<BaseEvalJob> {
        let root = self.repo_path.clone();
        let file_path = resolve_file_path(root.clone(), root, job_file.to_path_buf()).ok()?;
        Some(BaseEvalJob {
            file_path: file_path.to_string_lossy().into(),
            ci_info: self.ci_info.clone(),
        })
    }
}

impl RepoReader {
    /// Whether `job_name` of the commit in `ci_info` must be evaluated.
    ///
    /// A commit's job is evaluated once. The exception is a PR head whose
    /// jobset was diffed against another base than the PR's current one
    /// (the base branch moved or the PR was retargeted): that jobset's
    /// New/Changed classification is stale, so it is deleted and the head
    /// is evaluated and diffed again.
    pub(super) async fn needs_eval(
        &self,
        ci_info: &CICheckInfo,
        job_name: &str,
    ) -> anyhow::Result<bool> {
        let pool = &self.db_service.pool;
        if let Some(base_sha) = ci_info.base_commit.as_deref() {
            let stale = stale_head_jobset(
                &ci_info.commit,
                job_name,
                &ci_info.owner,
                &ci_info.repo_name,
                base_sha,
                pool,
            )
            .await?;
            if let Some(jobset_id) = stale {
                info!(
                    "Jobset {} of head {} job {} was diffed against another base; \
                     re-evaluating against base {}",
                    jobset_id, ci_info.commit, job_name, base_sha
                );
                delete_jobset(jobset_id, pool).await?;
                return Ok(true);
            }
        }
        let exists = self
            .db_service
            .has_jobset(
                &ci_info.commit,
                job_name,
                &ci_info.owner,
                &ci_info.repo_name,
            )
            .await?;
        Ok(!exists)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::RepoTask;
    use crate::nix::EvalTask;

    fn base_ci_info() -> CICheckInfo {
        CICheckInfo {
            commit: "base-sha".to_string(),
            base_commit: None,
            owner: "owner".to_string(),
            repo_name: "repo".to_string(),
            branch: Some("master".to_string()),
        }
    }

    #[test]
    fn eval_job_for_resolves_job_file_in_base_worktree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("ci")).unwrap();
        std::fs::write(dir.path().join("ci/jobs.nix"), "{}").unwrap();
        let checkout = BaseCheckout {
            repo_path: dir.path().canonicalize().unwrap(),
            ci_info: base_ci_info(),
        };

        let job = checkout.eval_job_for(Path::new("/ci/jobs.nix")).unwrap();

        let expected = dir.path().canonicalize().unwrap().join("ci/jobs.nix");
        assert_eq!(job.file_path, expected.to_string_lossy());
        assert_eq!(job.ci_info.commit, "base-sha");
    }

    #[test]
    fn eval_job_for_is_none_when_base_lacks_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = BaseCheckout {
            repo_path: dir.path().canonicalize().unwrap(),
            ci_info: base_ci_info(),
        };

        assert!(checkout.eval_job_for(Path::new("/ci/jobs.nix")).is_none());
    }

    /// Tasks journaled before `base` existed must still replay.
    #[test]
    fn journaled_tasks_without_base_deserialize() {
        let eval_json = r#"{"GithubJobPR":[{"file_path":"/w/ci.nix","name":"core",
            "allow_failures":true,"config_json":null},{"commit":"head",
            "base_commit":"base","owner":"o","repo_name":"r","branch":null}]}"#;
        let EvalTask::GithubJobPR((job, _)) = serde_json::from_str(eval_json).unwrap() else {
            panic!("expected GithubJobPR");
        };
        assert!(job.base.is_none());

        let repo_json = r#"{"ReadGitHub":{"repo_path":"/w","ci_info":{"commit":"head",
            "base_commit":"base","owner":"o","repo_name":"r","branch":null}}}"#;
        let RepoTask::ReadGitHub { base, .. } = serde_json::from_str(repo_json).unwrap() else {
            panic!("expected ReadGitHub");
        };
        assert!(base.is_none());
    }
}
