use anyhow::{Context, Result};
use octocrab::Octocrab;
use octocrab::models::CheckRunId;
use octocrab::models::checks::CheckRun;
use octocrab::models::reactions::ReactionContent;
use octocrab::params::checks::CheckRunConclusion;
use tracing::debug;

use crate::auth::types::GitHubPermission;
use crate::db::model::build_event::DrvBuildState;
use crate::github::CICheckInfo;
use crate::nix::nix_eval_jobs::NixEvalError;

/// This will send an initial ci gate which is used to determine what gates
/// are relevant for a PR
pub async fn create_ci_configure_gate(
    octocrab: &Octocrab,
    ci_check_info: &CICheckInfo,
) -> Result<CheckRun> {
    use octocrab::params::checks::CheckRunStatus;

    debug!(
        "Creating CI configure gate check run for commit {}",
        &ci_check_info.commit
    );

    // Create a check run for the CI configuration gate
    let check_run = octocrab
        //.installation(InstallationId(95084816))?
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .create_check_run("EkaCI: Configure", &ci_check_info.commit)
        .status(CheckRunStatus::InProgress)
        .send()
        .await?;

    debug!(
        "Successfully created CI configure gate check run for commit #{}",
        &ci_check_info.commit
    );

    Ok(check_run)
}

pub async fn update_ci_configure_gate(
    octocrab: &Octocrab,
    ci_check_info: &CICheckInfo,
    check_run_id: CheckRunId,
    status: octocrab::params::checks::CheckRunStatus,
    conclusion: CheckRunConclusion,
) -> Result<()> {
    debug!(
        "Updating CI configure gate check run {} with status {:?}",
        check_run_id, status
    );

    octocrab
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .update_check_run(check_run_id)
        .status(status)
        .conclusion(conclusion)
        .send()
        .await?;

    debug!(
        "Successfully updated CI configure gate check run {}",
        check_run_id
    );

    Ok(())
}

/// This will send an initial ci eval job gate which is used to determine what jobs
/// are being evaluated for a PR
pub async fn create_ci_eval_job(
    octocrab: &Octocrab,
    job_title: &str,
    ci_check_info: &CICheckInfo,
) -> Result<CheckRun> {
    use octocrab::params::checks::CheckRunStatus;

    debug!(
        "Creating CI eval job check run for commit {}",
        &ci_check_info.commit
    );

    let title = format!("EkaCI: Evaluate Job ({})", job_title);
    // Create a check run for the CI eval job gate
    let check_run = octocrab
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .create_check_run(&title, &ci_check_info.commit)
        .status(CheckRunStatus::InProgress)
        .send()
        .await?;

    debug!(
        "Successfully created CI eval job check run for commit #{}",
        &ci_check_info.commit
    );

    Ok(check_run)
}

pub async fn update_ci_eval_job(
    octocrab: &Octocrab,
    ci_check_info: &CICheckInfo,
    check_run_id: CheckRunId,
    status: octocrab::params::checks::CheckRunStatus,
    conclusion: CheckRunConclusion,
) -> Result<()> {
    debug!(
        "Updating CI eval job check run {} with status {:?}",
        check_run_id, status
    );

    octocrab
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .update_check_run(check_run_id)
        .status(status)
        .conclusion(conclusion)
        .send()
        .await?;

    debug!(
        "Successfully updated CI eval job check run {}",
        check_run_id
    );

    Ok(())
}

/// Create a neutral check run indicating that approval is required before builds can run
pub async fn create_approval_required_check_run(
    octocrab: &Octocrab,
    ci_check_info: &CICheckInfo,
    username: &str,
) -> Result<CheckRun> {
    use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

    debug!(
        "Creating approval required check run for commit {} (user: {})",
        &ci_check_info.commit, username
    );

    let title = format!("EkaCI: Approval Required (User: @{})", username);

    let check_run = octocrab
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .create_check_run(&title, &ci_check_info.commit)
        .status(CheckRunStatus::Completed)
        .conclusion(CheckRunConclusion::Neutral)
        .send()
        .await?;

    debug!(
        "Successfully created approval required check run for commit #{}",
        &ci_check_info.commit
    );

    Ok(check_run)
}

/// Fail a CI eval job gate due to evaluation errors
pub async fn fail_ci_eval_job(
    octocrab: &Octocrab,
    ci_check_info: &CICheckInfo,
    job_name: &str,
    errors: &[NixEvalError],
) -> Result<CheckRun> {
    use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

    debug!(
        "Creating failed CI eval job check run for job {} on commit {} with {} errors",
        job_name,
        &ci_check_info.commit,
        errors.len()
    );

    let title = format!("EkaCI: Evaluate Job ({})", job_name);

    // Format error details for the check run output
    let mut summary = format!(
        "# Evaluation Failed\n\n{} evaluation error(s) occurred:\n\n",
        errors.len()
    );

    for (idx, error) in errors.iter().enumerate() {
        summary.push_str(&format!("## Error {}\n\n", idx + 1));
        summary.push_str(&format!("**Attribute:** `{}`\n\n", error.attr));
        summary.push_str(&format!("**Error:**\n```\n{}\n```\n\n", error.error));
    }

    let check_run_output = octocrab::params::checks::CheckRunOutput {
        title: "Evaluation errors".to_string(),
        summary,
        text: None,
        annotations: vec![],
        images: vec![],
    };
    let check_run = octocrab
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .create_check_run(&title, &ci_check_info.commit)
        .status(CheckRunStatus::Completed)
        .conclusion(CheckRunConclusion::Failure)
        .output(check_run_output)
        .send()
        .await?;

    debug!(
        "Successfully created failed CI eval job check run for commit #{}",
        &ci_check_info.commit
    );

    Ok(check_run)
}

/// Create an in-progress check run for a check
pub async fn create_check_run(
    octocrab: &Octocrab,
    owner: &str,
    repo_name: &str,
    sha: &str,
    check_name: &str,
) -> Result<CheckRun> {
    use octocrab::params::checks::CheckRunStatus;

    debug!(
        "Creating check run '{}' for commit {} in {}/{}",
        check_name, sha, owner, repo_name
    );

    let title = format!("EkaCI: Check ({})", check_name);

    let check_run = octocrab
        .checks(owner, repo_name)
        .create_check_run(&title, sha)
        .status(CheckRunStatus::InProgress)
        .send()
        .await?;

    debug!(
        "Successfully created check run '{}' with ID {} for commit {}",
        check_name, check_run.id, sha
    );

    Ok(check_run)
}

/// Update a check run with completion status and output
pub async fn update_check_run(
    octocrab: &Octocrab,
    owner: &str,
    repo_name: &str,
    check_run_id: i64,
    check_name: &str,
    success: bool,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    duration_ms: i64,
) -> Result<()> {
    use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

    debug!(
        "Updating check run {} for check '{}' with success={}",
        check_run_id, check_name, success
    );

    let conclusion = if success {
        CheckRunConclusion::Success
    } else {
        CheckRunConclusion::Failure
    };

    // Format the check run output
    let mut summary = if success {
        format!(
            "# Check Passed ✓\n\nThe check `{}` completed successfully.\n\n",
            check_name
        )
    } else {
        format!(
            "# Check Failed ✗\n\nThe check `{}` failed with exit code {}.\n\n",
            check_name, exit_code
        )
    };

    summary.push_str(&format!("**Duration:** {}ms\n\n", duration_ms));

    // Include stdout and stderr if present
    let mut text = String::new();
    if !stdout.is_empty() {
        text.push_str("## Standard Output\n\n```\n");
        text.push_str(stdout);
        text.push_str("\n```\n\n");
    }
    if !stderr.is_empty() {
        text.push_str("## Standard Error\n\n```\n");
        text.push_str(stderr);
        text.push_str("\n```\n\n");
    }

    let check_run_output = octocrab::params::checks::CheckRunOutput {
        title: if success {
            "Check passed".to_string()
        } else {
            format!("Check failed (exit code {})", exit_code)
        },
        summary,
        text: if text.is_empty() { None } else { Some(text) },
        annotations: vec![],
        images: vec![],
    };

    octocrab
        .checks(owner, repo_name)
        .update_check_run(CheckRunId(check_run_id as u64))
        .status(CheckRunStatus::Completed)
        .conclusion(conclusion)
        .output(check_run_output)
        .send()
        .await?;

    debug!(
        "Successfully updated check run {} for check '{}'",
        check_run_id, check_name
    );

    Ok(())
}

/// Update a check run with size warning (neutral conclusion, warning message)
///
/// This is called when a build succeeds but exceeds the configured size threshold.
/// Sets the check to "neutral" (non-blocking) with a detailed warning message.
pub async fn update_check_run_with_size_warning(
    octocrab: &Octocrab,
    check_run: &crate::db::github::CheckRun,
    _status: &DrvBuildState,
    baseline_size: u64,
    current_size: u64,
    increase_percent: f64,
    threshold_percent: f64,
) -> Result<()> {
    use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

    debug!(
        "Updating check run {} with size warning: {}% increase (threshold: {}%)",
        check_run.check_run_id, increase_percent, threshold_percent
    );

    // Format sizes for display
    let baseline_formatted = crate::nix::size::format_size(baseline_size);
    let current_formatted = crate::nix::size::format_size(current_size);
    let diff_bytes = current_size as i64 - baseline_size as i64;
    let diff_formatted = if diff_bytes >= 0 {
        format!("+{}", crate::nix::size::format_size(diff_bytes as u64))
    } else {
        format!("-{}", crate::nix::size::format_size((-diff_bytes) as u64))
    };

    // Create detailed output
    let title = format!(
        "⚠️ Output size increased by {:.1}% (threshold: {:.1}%)",
        increase_percent, threshold_percent
    );

    let summary = format!(
        "The build output size has increased beyond the configured threshold.\n\n**Size \
         Comparison:**\n- **Baseline (main):** {}\n- **Current build:** {}\n- **Difference:** {} \
         ({:+.1}%)\n- **Threshold:** {:.1}%\n\nThis check is set to **neutral** (non-blocking) \
         but indicates potential installation bloat. Consider reviewing the changes to reduce \
         output size.",
        baseline_formatted, current_formatted, diff_formatted, increase_percent, threshold_percent
    );

    let check_run_output = octocrab::params::checks::CheckRunOutput {
        title,
        summary,
        text: None,
        annotations: vec![],
        images: vec![],
    };

    // Set status to Neutral (warning, non-blocking)
    let conclusion = CheckRunConclusion::Neutral;

    octocrab
        .checks(&check_run.repo_owner, &check_run.repo_name)
        .update_check_run(octocrab::models::CheckRunId(check_run.check_run_id as u64))
        .status(CheckRunStatus::Completed)
        .conclusion(conclusion)
        .output(check_run_output)
        .send()
        .await?;

    debug!(
        "Successfully updated check run {} with size warning",
        check_run.check_run_id
    );

    Ok(())
}

// ========================================
// Pull Request Merge Functions
// ========================================

/// Get PR reviews from GitHub
pub async fn get_pr_reviews(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Result<Vec<serde_json::Value>> {
    debug!(
        "Fetching reviews for PR #{} in {}/{}",
        pr_number, owner, repo
    );

    let reviews = octocrab
        .pulls(owner, repo)
        .list_reviews(pr_number)
        .send()
        .await?;

    // Convert to JSON values
    let json_reviews: Vec<serde_json::Value> = reviews
        .items
        .into_iter()
        .filter_map(|r| serde_json::to_value(r).ok())
        .collect();

    Ok(json_reviews)
}

/// Get allowed merge methods for a repository from GitHub settings
pub async fn get_repository_merge_settings(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
) -> Result<Vec<String>> {
    debug!("Fetching repository merge settings for {}/{}", owner, repo);

    let repo_info = octocrab.repos(owner, repo).get().await?;

    let mut allowed_methods = Vec::new();

    // Check which merge methods are allowed
    if repo_info.allow_merge_commit.unwrap_or(true) {
        allowed_methods.push("merge".to_string());
    }
    if repo_info.allow_squash_merge.unwrap_or(true) {
        allowed_methods.push("squash".to_string());
    }
    if repo_info.allow_rebase_merge.unwrap_or(true) {
        allowed_methods.push("rebase".to_string());
    }

    debug!(
        "Repository {}/{} allows merge methods: {:?}",
        owner, repo, allowed_methods
    );

    Ok(allowed_methods)
}

/// Outcome of validating a desired merge method against a repository's
/// settings. Callers distinguish between `Ok` (method is allowed) and
/// `NotAllowed` (method is disabled on the repo) so they can surface a
/// proper error to the user or fall back as appropriate. Network/API
/// failures surface as `Err` from [`validate_merge_method`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeMethodCheck {
    /// The requested method is one of the repository's allowed merge methods.
    Ok,
    /// The requested method is not allowed; the enclosed vector lists the
    /// methods the repository _does_ permit (possibly empty).
    NotAllowed { allowed: Vec<String> },
}

/// Validate that `merge_method` is permitted by the target repository's
/// merge settings.
///
/// This should be called before [`merge_pull_request`] to avoid avoidable
/// merge failures (e.g. asking GitHub to squash-merge a repo that has
/// squash merges disabled). Returns `Err` only if the repository metadata
/// could not be fetched.
pub async fn validate_merge_method(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    merge_method: &str,
) -> Result<MergeMethodCheck> {
    let allowed = get_repository_merge_settings(octocrab, owner, repo).await?;
    if allowed.iter().any(|m| m == merge_method) {
        Ok(MergeMethodCheck::Ok)
    } else {
        Ok(MergeMethodCheck::NotAllowed { allowed })
    }
}

/// Merge a pull request using the specified merge method
pub async fn merge_pull_request(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
    merge_method: &str,
    commit_title: Option<String>,
    commit_message: Option<String>,
) -> Result<()> {
    use octocrab::params::pulls::MergeMethod;

    debug!(
        "Merging PR #{} in {}/{} with method '{}'",
        pr_number, owner, repo, merge_method
    );

    let method = match merge_method {
        "merge" => MergeMethod::Merge,
        "squash" => MergeMethod::Squash,
        "rebase" => MergeMethod::Rebase,
        _ => {
            debug!(
                "Invalid merge method '{}', defaulting to squash",
                merge_method
            );
            MergeMethod::Squash
        },
    };

    let pulls = octocrab.pulls(owner, repo);
    let mut merge_builder = pulls.merge(pr_number).method(method);

    if let Some(ref title) = commit_title {
        merge_builder = merge_builder.title(title);
    }

    if let Some(ref message) = commit_message {
        merge_builder = merge_builder.message(message);
    }

    merge_builder.send().await?;

    debug!(
        "Successfully merged PR #{} in {}/{}",
        pr_number, owner, repo
    );

    Ok(())
}

/// Check if a PR has maintainer approvals for all changed packages
/// Returns (eligible, missing_approvals) where:
/// - eligible: true if all packages have at least one maintainer approval
/// - missing_approvals: Map of package -> list of maintainer usernames (for debugging)
pub async fn check_pr_maintainer_approvals(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    pr_number: u64,
    changed_packages: &[String],
    pool: &sqlx::Pool<sqlx::Sqlite>,
) -> Result<(bool, std::collections::HashMap<String, Vec<String>>)> {
    use std::collections::{HashMap, HashSet};

    if changed_packages.is_empty() {
        return Ok((false, HashMap::new()));
    }

    // Get PR reviews from GitHub
    let reviews = get_pr_reviews(octocrab, owner, repo, pr_number).await?;

    // Filter to only approved reviews
    let approved_reviews: Vec<_> = reviews
        .iter()
        .filter(|r| {
            r.get("state")
                .and_then(|s| s.as_str())
                .map(|s| s == "APPROVED")
                .unwrap_or(false)
        })
        .collect();

    if approved_reviews.is_empty() {
        // No approvals at all
        debug!("PR #{} has no approved reviews", pr_number);
        let mut missing = HashMap::new();
        for package in changed_packages {
            missing.insert(package.clone(), vec![]);
        }
        return Ok((false, missing));
    }

    // Get list of approving users
    let approving_usernames: HashSet<_> = approved_reviews
        .iter()
        .filter_map(|r| {
            r.get("user")
                .and_then(|u| u.get("login"))
                .and_then(|l| l.as_str())
        })
        .collect();

    debug!(
        "PR #{} has approvals from: {:?}",
        pr_number, approving_usernames
    );

    // For each package, check if at least one approver is a maintainer
    let mut all_packages_approved = true;
    let mut missing_approvals: HashMap<String, Vec<String>> = HashMap::new();

    for package in changed_packages {
        // Get maintainers for this package
        let maintainers =
            crate::db::maintainers::get_maintainers_for_attr_path(package, pool).await?;

        let maintainer_usernames: Vec<String> = maintainers
            .iter()
            .map(|m| m.github_username.clone())
            .collect();

        // Check if any approver is a maintainer
        let has_maintainer_approval = maintainer_usernames
            .iter()
            .any(|m| approving_usernames.contains(m.as_str()));

        if !has_maintainer_approval {
            all_packages_approved = false;
            missing_approvals.insert(package.clone(), maintainer_usernames);
        }
    }

    debug!(
        "PR #{} eligibility: {} (missing approvals: {:?})",
        pr_number, all_packages_approved, missing_approvals
    );

    Ok((all_packages_approved, missing_approvals))
}

// ========================================
// Dependency Changes Gate Functions
// ========================================

/// Create a neutral check run showing dependency changes for packages in the PR
///
/// This gate is informational (non-blocking) and displays which dependencies
/// were added or removed for each package that had dependency changes.
pub async fn create_dependency_changes_gate(
    octocrab: &Octocrab,
    ci_check_info: &CICheckInfo,
    dependency_diff: &str,
    total_packages_with_changes: usize,
) -> Result<CheckRun> {
    use octocrab::params::checks::{CheckRunConclusion, CheckRunStatus};

    debug!(
        "Creating dependency changes gate for commit {} with {} packages affected",
        &ci_check_info.commit, total_packages_with_changes
    );

    let title = "EkaCI: Dependency Changes";

    // Build the summary with diff-style formatting
    let summary = if total_packages_with_changes == 0 {
        "# No Dependency Changes\n\nNo packages have dependency changes in this PR.\n\n**Status:** \
         Informational (non-blocking)"
            .to_string()
    } else {
        format!(
            "# Dependency Changes Detected\n\n**Packages affected:** {}\n\n**Status:** \
             Informational (non-blocking)\n\n---\n\n{}",
            total_packages_with_changes, dependency_diff
        )
    };

    let check_run_output = octocrab::params::checks::CheckRunOutput {
        title: if total_packages_with_changes == 0 {
            "No dependency changes".to_string()
        } else {
            format!(
                "{} package(s) with dependency changes",
                total_packages_with_changes
            )
        },
        summary,
        text: None,
        annotations: vec![],
        images: vec![],
    };

    let check_run = octocrab
        .checks(&ci_check_info.owner, &ci_check_info.repo_name)
        .create_check_run(title, &ci_check_info.commit)
        .status(CheckRunStatus::Completed)
        .conclusion(CheckRunConclusion::Neutral) // Non-blocking
        .output(check_run_output)
        .send()
        .await?;

    debug!(
        "Successfully created dependency changes gate check run #{} for commit {}",
        check_run.id, &ci_check_info.commit
    );

    Ok(check_run)
}

// ---- Issue comments, reactions, and permission checks ----
//
// Helpers for the `@eka-ci merge` comment command. All require an
// installation-authenticated `Octocrab` (i.e. `octocrab_for_owner`).

/// Map a reaction string to `ReactionContent`; unknown → `Eyes`
/// (neutral) so typos don't fail the call site.
fn reaction_from_str(content: &str) -> ReactionContent {
    match content {
        "+1" => ReactionContent::PlusOne,
        "-1" => ReactionContent::MinusOne,
        "rocket" => ReactionContent::Rocket,
        "confused" => ReactionContent::Confused,
        "laugh" => ReactionContent::Laugh,
        "heart" => ReactionContent::Heart,
        "hooray" => ReactionContent::Hooray,
        _ => ReactionContent::Eyes,
    }
}

/// Add an emoji reaction to an issue/PR comment. See
/// `docs/github-pr-comment-support.md` for the reaction legend.
pub async fn add_comment_reaction(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    comment_id: i64,
    content: &str,
) -> Result<()> {
    let reaction = reaction_from_str(content);
    debug!(
        "Adding reaction {:?} to comment {} in {}/{}",
        reaction, comment_id, owner, repo
    );
    octocrab
        .issues(owner, repo)
        .create_comment_reaction(comment_id as u64, reaction)
        .await
        .with_context(|| format!("failed to add reaction to comment {}", comment_id))?;
    Ok(())
}

/// Post a new issue/PR comment.
pub async fn post_issue_comment(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    issue_number: i64,
    body: &str,
) -> Result<()> {
    debug!(
        "Posting comment on {}/{}#{} ({} bytes)",
        owner,
        repo,
        issue_number,
        body.len()
    );
    octocrab
        .issues(owner, repo)
        .create_comment(issue_number as u64, body)
        .await
        .with_context(|| {
            format!(
                "failed to post comment on {}/{}#{}",
                owner, repo, issue_number
            )
        })?;
    Ok(())
}

/// Response shape of `GET /repos/{owner}/{repo}/collaborators/{username}/permission`.
/// `role_name` is finer-grained (admin|maintain|write|triage|read) than
/// `permission` (admin|write|read|none); prefer it when present.
#[derive(Debug, serde::Deserialize)]
struct CollaboratorPermissionResponse {
    permission: String,
    #[serde(default)]
    role_name: Option<String>,
}

/// Check an arbitrary user's permission on a repo using the App
/// installation token. Unlike `auth::github_api::check_repo_permission`
/// (OAuth, self-only), this hits the collaborators endpoint which
/// requires push access. 404 → `None`.
pub async fn check_repo_permission_for_user(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    username: &str,
) -> Result<GitHubPermission> {
    let route = format!(
        "/repos/{}/{}/collaborators/{}/permission",
        owner, repo, username
    );

    let resp: std::result::Result<CollaboratorPermissionResponse, octocrab::Error> =
        octocrab.get(&route, None::<&()>).await;

    let resp = match resp {
        Ok(r) => r,
        // Treat "not a collaborator" as None rather than an error.
        Err(octocrab::Error::GitHub {
            source,
            backtrace: _,
        }) if source.status_code == http::StatusCode::NOT_FOUND => {
            return Ok(GitHubPermission::None);
        },
        Err(e) => {
            return Err(anyhow::anyhow!(
                "failed to fetch collaborator permission for {} in {}/{}: {:?}",
                username,
                owner,
                repo,
                e
            ));
        },
    };

    // Prefer `role_name`; fall back to `permission`. Same enum as the
    // user-token path so downstream gates treat both modes identically.
    let role = resp
        .role_name
        .as_deref()
        .unwrap_or(resp.permission.as_str());
    let perm = match role {
        "admin" => GitHubPermission::Admin,
        "maintain" => GitHubPermission::Maintain,
        "write" => GitHubPermission::Write,
        "triage" => GitHubPermission::Triage,
        "read" => GitHubPermission::Read,
        _ => GitHubPermission::None,
    };
    debug!(
        "collaborator permission for {} on {}/{}: {:?}",
        username, owner, repo, perm
    );
    Ok(perm)
}

/// Fetch a commit's committer date (best-effort force-push signal for
/// the `@eka-ci merge` handler). `Ok(None)` for missing/unparseable
/// dates — treat as "unknown, accept". Cherry-picked old commits won't
/// trigger the gate; the sync-drift hook is the backstop.
pub async fn fetch_head_commit_date(
    octocrab: &Octocrab,
    owner: &str,
    repo: &str,
    sha: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
    let route = format!("/repos/{}/{}/commits/{}", owner, repo, sha);

    let commit: octocrab::models::commits::Commit = octocrab
        .get(&route, None::<&()>)
        .await
        .with_context(|| format!("fetching commit {}@{}/{}", sha, owner, repo))?;

    // `committer.date` absent is unusual but valid per the spec.
    let Some(date_str) = commit.commit.committer.and_then(|g| g.date) else {
        debug!("commit {}@{}/{} has no committer date", sha, owner, repo);
        return Ok(None);
    };

    match chrono::DateTime::parse_from_rfc3339(&date_str) {
        Ok(dt) => Ok(Some(dt.with_timezone(&chrono::Utc))),
        Err(e) => {
            debug!(
                "commit {}@{}/{} committer date {:?} failed to parse: {:?}",
                sha, owner, repo, date_str, e
            );
            Ok(None)
        },
    }
}
