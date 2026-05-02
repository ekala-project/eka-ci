// Authorization logic for merge commands

use anyhow::Result;
use octocrab::Octocrab;
use tracing::warn;

use super::super::GitHubService;
use crate::github::service::actions;

/// Result of the commenter authorization check.
pub(super) enum Authorization {
    /// Commenter is authorized. `has_write` distinguishes the "repo
    /// write" path from the "package maintainer" path (kept for logging).
    Granted {
        #[allow(dead_code)]
        has_write: bool,
    },
    /// Commenter is not authorized. Caller should react `-1` + post
    /// explanation comment.
    Denied,
    /// Permission lookup failed; caller should drop the command silently.
    Abort,
}

impl GitHubService {
    /// Outcome of an authorization check against a commenter.
    pub(super) async fn authorize_commenter(
        &self,
        octocrab: &Octocrab,
        owner: &str,
        repo_name: &str,
        pr_number: i64,
        requester_id: i64,
        requester_login: &str,
    ) -> Result<Authorization> {
        let perm = match actions::check_repo_permission_for_user(
            octocrab,
            owner,
            repo_name,
            requester_login,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                warn!(
                    "Failed to check repo permission for {} on {}/{}: {:?}",
                    requester_login, owner, repo_name, e
                );
                return Ok(Authorization::Abort);
            },
        };
        let has_write = matches!(
            perm,
            crate::auth::types::GitHubPermission::Admin
                | crate::auth::types::GitHubPermission::Maintain
                | crate::auth::types::GitHubPermission::Write
        );
        if has_write {
            return Ok(Authorization::Granted { has_write: true });
        }

        let changed = crate::db::github::get_pr_changed_packages(
            pr_number,
            owner,
            repo_name,
            &self.db_service.pool,
        )
        .await
        .unwrap_or_default();
        let is_pkg_maintainer = if changed.is_empty() {
            false
        } else {
            crate::db::maintainers::is_maintainer_of_all_packages(
                requester_id,
                &changed,
                &self.db_service.pool,
            )
            .await
            .unwrap_or(false)
        };

        if is_pkg_maintainer {
            Ok(Authorization::Granted { has_write: false })
        } else {
            Ok(Authorization::Denied)
        }
    }
}
