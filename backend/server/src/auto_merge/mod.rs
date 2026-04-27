///! Shared auto-merge eligibility logic.
///!
///! This module contains platform-agnostic rules for when a PR/MR is eligible
///! for auto-merge. Platform services (GitHub, GitLab, Gitea) use these
///! functions to evaluate eligibility, then handle platform-specific API calls.

/// Represents the state of a merge request's auto-merge eligibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeEligibility {
    /// PR/MR is eligible for auto-merge.
    Eligible { merge_method: String },
    /// Head build not yet complete.
    BuildInProgress,
    /// PR/MR not found in database.
    NotFound,
    /// Head SHA has drifted from pinned commit (comment-merge only).
    SHADrift { expected: String, actual: String },
    /// No auto-merge or comment-merge enabled.
    NotEnabled,
    /// No changed packages detected.
    NoChangedPackages,
    /// Missing required maintainer approvals.
    MissingApprovals { packages: Vec<String> },
    /// Merge method not allowed by repository settings.
    MethodNotAllowed {
        requested: String,
        allowed: Vec<String>,
    },
}

/// State of auto-merge for a PR/MR.
#[derive(Debug, Clone)]
pub struct AutoMergeState {
    /// Auto-merge enabled via PR/MR settings
    pub auto_merge_enabled: bool,
    /// Pending comment-triggered merge request
    pub comment_merge: Option<CommentMergeRequest>,
    /// Head commit SHA
    pub head_sha: String,
    /// Build succeeded for head commit
    pub build_succeeded: bool,
    /// List of changed package names
    pub changed_packages: Vec<String>,
    /// Missing maintainer approvals (package names)
    pub missing_approvals: Vec<String>,
    /// Preferred merge method from PR/MR or comment
    pub merge_method: Option<String>,
    /// Allowed merge methods by repository settings
    pub allowed_merge_methods: Vec<String>,
}

/// Comment-triggered merge request details.
#[derive(Debug, Clone)]
pub struct CommentMergeRequest {
    /// SHA pinned by the comment
    pub sha: String,
    /// Merge method requested (if specified)
    pub method: Option<String>,
}

impl AutoMergeState {
    /// Evaluate merge eligibility based on current state.
    ///
    /// This implements the business logic for determining if a PR/MR can be
    /// auto-merged. Platform services call this after gathering all state,
    /// then handle platform-specific merge API calls if eligible.
    pub fn evaluate_eligibility(&self) -> MergeEligibility {
        // Check 1: Build must have succeeded
        if !self.build_succeeded {
            return MergeEligibility::BuildInProgress;
        }

        // Check 2: Handle comment-merge SHA drift
        if let Some(ref cmr) = self.comment_merge {
            if cmr.sha != self.head_sha {
                return MergeEligibility::SHADrift {
                    expected: cmr.sha.clone(),
                    actual: self.head_sha.clone(),
                };
            }
        }

        // Check 3: At least one merge path must be active
        if !self.auto_merge_enabled && self.comment_merge.is_none() {
            return MergeEligibility::NotEnabled;
        }

        // Check 4: Must have changed packages
        if self.changed_packages.is_empty() {
            return MergeEligibility::NoChangedPackages;
        }

        // Check 5: Maintainer approvals (skipped for comment-merge, since
        // authority was verified when processing the comment command)
        if self.comment_merge.is_none() && !self.missing_approvals.is_empty() {
            return MergeEligibility::MissingApprovals {
                packages: self.missing_approvals.clone(),
            };
        }

        // Check 6: Determine merge method
        let merge_method = self
            .comment_merge
            .as_ref()
            .and_then(|cmr| cmr.method.clone())
            .or_else(|| self.merge_method.clone())
            .unwrap_or_else(|| "squash".to_string());

        // Check 7: Validate merge method against allowed methods
        if !self.allowed_merge_methods.is_empty()
            && !self.allowed_merge_methods.contains(&merge_method)
        {
            return MergeEligibility::MethodNotAllowed {
                requested: merge_method,
                allowed: self.allowed_merge_methods.clone(),
            };
        }

        MergeEligibility::Eligible { merge_method }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_state() -> AutoMergeState {
        AutoMergeState {
            auto_merge_enabled: true,
            comment_merge: None,
            head_sha: "abc123".to_string(),
            build_succeeded: true,
            changed_packages: vec!["hello".to_string()],
            missing_approvals: vec![],
            merge_method: Some("squash".to_string()),
            allowed_merge_methods: vec!["squash".to_string(), "merge".to_string()],
        }
    }

    #[test]
    fn eligible_when_all_checks_pass() {
        let state = base_state();
        assert_eq!(
            state.evaluate_eligibility(),
            MergeEligibility::Eligible {
                merge_method: "squash".to_string()
            }
        );
    }

    #[test]
    fn not_eligible_when_build_in_progress() {
        let mut state = base_state();
        state.build_succeeded = false;
        assert_eq!(
            state.evaluate_eligibility(),
            MergeEligibility::BuildInProgress
        );
    }

    #[test]
    fn sha_drift_when_comment_merge_sha_differs() {
        let mut state = base_state();
        state.comment_merge = Some(CommentMergeRequest {
            sha: "def456".to_string(),
            method: None,
        });
        assert_eq!(
            state.evaluate_eligibility(),
            MergeEligibility::SHADrift {
                expected: "def456".to_string(),
                actual: "abc123".to_string()
            }
        );
    }

    #[test]
    fn not_enabled_when_no_auto_merge_or_comment() {
        let mut state = base_state();
        state.auto_merge_enabled = false;
        assert_eq!(state.evaluate_eligibility(), MergeEligibility::NotEnabled);
    }

    #[test]
    fn no_changed_packages_blocks_merge() {
        let mut state = base_state();
        state.changed_packages = vec![];
        assert_eq!(
            state.evaluate_eligibility(),
            MergeEligibility::NoChangedPackages
        );
    }

    #[test]
    fn missing_approvals_blocks_auto_merge_not_comment() {
        let mut state = base_state();
        state.missing_approvals = vec!["hello".to_string()];
        assert_eq!(
            state.evaluate_eligibility(),
            MergeEligibility::MissingApprovals {
                packages: vec!["hello".to_string()]
            }
        );
    }

    #[test]
    fn missing_approvals_allowed_for_comment_merge() {
        let mut state = base_state();
        state.missing_approvals = vec!["hello".to_string()];
        state.comment_merge = Some(CommentMergeRequest {
            sha: "abc123".to_string(),
            method: None,
        });
        // Should be eligible despite missing approvals
        assert_eq!(
            state.evaluate_eligibility(),
            MergeEligibility::Eligible {
                merge_method: "squash".to_string()
            }
        );
    }

    #[test]
    fn method_not_allowed_blocks_merge() {
        let mut state = base_state();
        state.merge_method = Some("rebase".to_string());
        assert_eq!(
            state.evaluate_eligibility(),
            MergeEligibility::MethodNotAllowed {
                requested: "rebase".to_string(),
                allowed: vec!["squash".to_string(), "merge".to_string()]
            }
        );
    }

    #[test]
    fn comment_merge_method_takes_precedence() {
        let mut state = base_state();
        state.merge_method = Some("squash".to_string());
        state.comment_merge = Some(CommentMergeRequest {
            sha: "abc123".to_string(),
            method: Some("merge".to_string()),
        });
        assert_eq!(
            state.evaluate_eligibility(),
            MergeEligibility::Eligible {
                merge_method: "merge".to_string()
            }
        );
    }
}
