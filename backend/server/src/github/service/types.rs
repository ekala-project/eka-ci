use std::fmt;
use std::sync::Arc;

use anyhow::{Context, Result};
use octocrab::Octocrab;
use octocrab::models::checks::CheckRun;
use octocrab::models::pulls::PullRequest;
use octocrab::params::checks::{CheckRunConclusion as GHConclusion, CheckRunStatus as GHStatus};
use serde::Serialize;

use crate::checks::types::CheckResultMessage;
use crate::db::model::DrvId;
use crate::db::model::build_event::DrvBuildState;
use crate::nix::nix_eval_jobs::{NixEvalDrv, NixEvalError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum JobDifference {
    New,
    Changed,
    Removed,
}

impl fmt::Display for JobDifference {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            JobDifference::New => write!(f, "New"),
            JobDifference::Changed => write!(f, "Changed"),
            JobDifference::Removed => write!(f, "Removed"),
        }
    }
}

// SQLx encoding/decoding implementation
mod job_difference_encoding {
    use sqlx::{Decode, Encode, Sqlite, Type};

    use super::JobDifference;

    #[derive(sqlx::Type)]
    #[repr(i64)]
    enum JobDifferenceRepr {
        New = 0,
        Changed = 1,
        Removed = 2,
    }

    impl From<&JobDifference> for JobDifferenceRepr {
        fn from(value: &JobDifference) -> Self {
            match value {
                JobDifference::New => Self::New,
                JobDifference::Changed => Self::Changed,
                JobDifference::Removed => Self::Removed,
            }
        }
    }

    impl From<JobDifferenceRepr> for JobDifference {
        fn from(value: JobDifferenceRepr) -> Self {
            match value {
                JobDifferenceRepr::New => Self::New,
                JobDifferenceRepr::Changed => Self::Changed,
                JobDifferenceRepr::Removed => Self::Removed,
            }
        }
    }

    impl<'q> Encode<'q, Sqlite> for JobDifference {
        fn encode_by_ref(
            &self,
            buf: &mut <Sqlite as sqlx::Database>::ArgumentBuffer<'q>,
        ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
            <JobDifferenceRepr as Encode<'q, Sqlite>>::encode_by_ref(&self.into(), buf)
        }

        fn size_hint(&self) -> usize {
            <JobDifferenceRepr as Encode<'q, Sqlite>>::size_hint(&self.into())
        }
    }

    impl<'r> Decode<'r, Sqlite> for JobDifference {
        fn decode(
            value: <Sqlite as sqlx::Database>::ValueRef<'r>,
        ) -> Result<Self, sqlx::error::BoxDynError> {
            Ok(<JobDifferenceRepr as Decode<Sqlite>>::decode(value)?.into())
        }
    }

    impl Type<Sqlite> for JobDifference {
        fn type_info() -> <Sqlite as sqlx::Database>::TypeInfo {
            <JobDifferenceRepr as Type<Sqlite>>::type_info()
        }

        fn compatible(ty: &<Sqlite as sqlx::Database>::TypeInfo) -> bool {
            <JobDifferenceRepr as Type<Sqlite>>::compatible(ty)
        }
    }
}

#[derive(Debug, Clone)]
/// Information needed to create a CI check run gate
pub struct CICheckInfo {
    pub commit: String,
    pub base_commit: Option<String>,
    pub owner: String,
    pub repo_name: String,
}

impl CICheckInfo {
    pub fn from_gh_pr_base(pr: &PullRequest) -> anyhow::Result<Self> {
        let commit = pr.base.sha.clone();
        let repo = pr
            .base
            .repo
            .as_ref()
            .context("PR base has no repository information")?;
        let owner = repo
            .owner
            .as_ref()
            .context("PR base repository has no owner information")?
            .login
            .clone();
        let repo_name = repo.name.clone();

        Ok(Self {
            commit,
            base_commit: None,
            owner,
            repo_name,
        })
    }

    pub fn from_gh_pr_head(pr: &PullRequest) -> anyhow::Result<Self> {
        let commit = pr.head.sha.clone();
        let base_commit = pr.base.sha.clone();
        let repo = pr
            .head
            .repo
            .as_ref()
            .context("PR head has no repository information")?;
        let owner = repo
            .owner
            .as_ref()
            .context("PR head repository has no owner information")?
            .login
            .clone();
        let repo_name = repo.name.clone();

        Ok(Self {
            commit,
            base_commit: Some(base_commit),
            owner,
            repo_name,
        })
    }

    pub async fn create_gh_check_run(
        &self,
        octocrab: &Octocrab,
        jobset_name: &str,
        name: &str,
        initial_status: DrvBuildState,
        difference: &JobDifference,
    ) -> Result<CheckRun> {
        let title = format!("{} / {} ({})", name, difference, jobset_name);
        let (gh_status, gh_conclusion) = match difference {
            // If it's been removed, we don't really care what the previous status was
            JobDifference::Removed => (GHStatus::Completed, Some(GHConclusion::Neutral)),
            _ => initial_status.as_gh_checkrun_state(),
        };
        self.inner_gh_check_run(octocrab, &title, gh_status, gh_conclusion)
            .await
    }

    async fn inner_gh_check_run(
        &self,
        octocrab: &Octocrab,
        title: &str,
        gh_status: GHStatus,
        gh_conclusion: Option<GHConclusion>,
    ) -> Result<CheckRun> {
        let check_builder = octocrab.checks(&self.owner, &self.repo_name);
        let mut create_check_run = check_builder
            .create_check_run(title, &self.commit)
            .status(gh_status);

        if let Some(conclusion) = gh_conclusion {
            create_check_run = create_check_run.conclusion(conclusion);
        }

        let check_run = create_check_run.send().await?;
        Ok(check_run)
    }
}

/// `drv_id` and `ci_check_info` fields carry `Arc<_>` so fan-out call sites
/// (e.g. the recorder's per-jobset loop or the ingress path creating multiple
/// check-runs per commit) can `Arc::clone` refcount bumps instead of cloning
/// the inner `DrvId` string or the 4-String `CICheckInfo`.
#[derive(Debug)]
pub enum GitHubTask {
    UpdateBuildStatus {
        drv_id: Arc<DrvId>,
        status: DrvBuildState,
    },
    UpdateBuildStatusWithSizeWarning {
        drv_id: Arc<DrvId>,
        status: DrvBuildState,
        baseline_size: u64,
        current_size: u64,
        increase_percent: f64,
        threshold_percent: f64,
    },
    CreateJobSet {
        ci_check_info: Arc<CICheckInfo>,
        name: String,
        jobs: Vec<NixEvalDrv>,
        config_json: Option<String>, // Serialized job config for hooks
    },
    CreateCIConfigureGate {
        ci_check_info: Arc<CICheckInfo>,
    },
    CompleteCIConfigureGate {
        ci_check_info: Arc<CICheckInfo>,
    },
    CreateCIEvalJob {
        ci_check_info: Arc<CICheckInfo>,
        job_title: String,
    },
    CompleteCIEvalJob {
        ci_check_info: Arc<CICheckInfo>,
        job_name: String,
        conclusion: octocrab::params::checks::CheckRunConclusion,
    },
    CancelCheckRunsForCommit {
        ci_check_info: Arc<CICheckInfo>,
    },
    CreateFailureCheckRun {
        drv_id: Arc<DrvId>,
        jobset_id: i64,
        job_attr_name: String,
        difference: JobDifference,
    },
    CreateApprovalRequiredCheckRun {
        ci_check_info: Arc<CICheckInfo>,
        username: String,
    },
    FailCIEvalJob {
        ci_check_info: Arc<CICheckInfo>,
        job_name: String,
        errors: Vec<NixEvalError>,
    },
    CreateCheckRun {
        owner: String,
        repo_name: String,
        sha: String,
        check_name: String,
        check_result_id: i64,
    },
    CheckComplete(CheckResultMessage),
    CheckFailed(CheckResultMessage),
    CheckAutoMerge {
        owner: String,
        repo_name: String,
        pr_number: i64,
    },
    CreateDependencyChangesGate {
        ci_check_info: Arc<CICheckInfo>,
        jobset_id: i64,
        base_jobset_id: i64,
    },
    /// Handle an `@eka-ci merge …` comment on a PR: verify authorization,
    /// pin the current head SHA, record a pending comment-merge.
    ProcessMergeCommand {
        owner: String,
        repo_name: String,
        pr_number: i64,
        comment_id: i64,
        requester_id: i64,
        requester_login: String,
        /// Raw comment body; re-parsed on the handler side.
        body: String,
        /// Comment creation time from the webhook. Used by the push-time
        /// gate to detect commits pushed after the command was issued.
        comment_created_at: chrono::DateTime<chrono::Utc>,
    },
    /// Notify a requester that their pending comment-merge was cancelled
    /// due to head-SHA drift.
    CommentMergeDriftCancelled {
        owner: String,
        repo_name: String,
        pr_number: i64,
        expected_sha: String,
        actual_sha: String,
        requester_login: String,
    },
    /// React to an issue comment (`+1`, `-1`, `rocket`, `confused`, …).
    ReactToComment {
        owner: String,
        repo_name: String,
        comment_id: i64,
        /// GitHub reaction content string: `+1` | `-1` | `laugh` |
        /// `confused` | `heart` | `hooray` | `rocket` | `eyes`.
        content: &'static str,
    },
    /// Post an issue/PR comment. Currently unused; kept for future call
    /// sites (e.g., queued merge-failure explanations).
    #[allow(dead_code)]
    PostIssueComment {
        owner: String,
        repo_name: String,
        issue_number: i64,
        body: String,
    },
}

pub type Owner = String;
pub type Commit = String;
