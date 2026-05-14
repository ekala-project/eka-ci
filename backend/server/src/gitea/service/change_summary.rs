// Change summary generation and posting for Gitea PRs

use std::sync::Arc;

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::gitea::GiteaClient;
use crate::gitea::types::GiteaCIInfo;
use crate::graph::GraphServiceHandle;
use crate::metrics::ChangeSummaryMetrics;

/// Post (or update) change summary check for a PR head
/// Uses check runs API for newer Gitea versions, commit statuses for older
pub(super) async fn handle_create_change_summary_check(
    ci_info: &Arc<GiteaCIInfo>,
    job: &str,
    client: &GiteaClient,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    graph_handle: &GraphServiceHandle,
    change_summary_metrics: Option<&Arc<ChangeSummaryMetrics>>,
) -> Result<()> {
    let Some(base_sha) = ci_info.base_commit.as_deref() else {
        debug!(
            "Skipping change-summary for {}: no base commit (not a PR head)",
            &ci_info.commit
        );
        return Ok(());
    };

    // Resolve head jobset ID from GiteaJobSets
    let head_jobset: Option<i64> = sqlx::query_scalar(
        "SELECT ROWID FROM GiteaJobSets WHERE sha = ? AND job = ? AND domain = ?",
    )
    .bind(&ci_info.commit)
    .bind(job)
    .bind(&ci_info.domain)
    .fetch_optional(db_pool)
    .await?;

    let Some(head_jobset_id) = head_jobset else {
        debug!(
            "No head jobset for sha={} job={} domain={}; skipping change-summary",
            &ci_info.commit, job, &ci_info.domain
        );
        return Ok(());
    };

    // Resolve base jobset ID if it exists
    let base_jobset_id: Option<i64> = sqlx::query_scalar(
        "SELECT ROWID FROM GiteaJobSets WHERE sha = ? AND job = ? AND domain = ?",
    )
    .bind(base_sha)
    .bind(job)
    .bind(&ci_info.domain)
    .fetch_optional(db_pool)
    .await?;

    // Create JobsetData for the head commit
    let jobset_data = crate::JobsetData::new(
        &ci_info.owner,
        &ci_info.repo_name,
        &ci_info.domain,
        &ci_info.commit,
        job,
        None,
    );

    // Resolve options from jobset data
    let (opts, status) = crate::change_summary_compat::resolve_options_from_jobset_data(
        &jobset_data,
        change_summary_metrics.map(|m| m.as_ref()),
    )
    .await;

    // Build change summary using platform-agnostic function
    // TODO: Pass actual metrics when change_summary crate supports server's metrics type
    let summary = match crate::change_summary::build_change_summary_from_jobset_ids(
        db_pool,
        graph_handle,
        head_jobset_id,
        base_jobset_id,
        &jobset_data,
        base_sha,
        &opts,
        &status,
        None, // metrics not supported yet
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            warn!(
                "Failed to build change-summary for commit {}: {:?}",
                &ci_info.commit, e
            );
            return Ok(());
        },
    };

    let markdown = summary.markdown;

    // Look up the PR by head SHA to get the PR number
    let pr = match crate::db::gitea::get_pr_by_head_sha(
        &ci_info.commit,
        &ci_info.domain,
        &ci_info.owner,
        &ci_info.repo_name,
        db_pool,
    )
    .await
    {
        Ok(Some(pr)) => pr,
        Ok(None) => {
            debug!(
                "No PR found for commit {} in {}/{}/{}; skipping change-summary",
                &ci_info.commit, ci_info.domain, ci_info.owner, ci_info.repo_name
            );
            return Ok(());
        },
        Err(e) => {
            warn!(
                "Failed to look up PR for commit {}: {:?}",
                &ci_info.commit, e
            );
            return Ok(());
        },
    };

    // For Gitea, we can post the change summary as a PR comment
    // Similar to GitLab, we use a marker to identify and update our comment
    let marker = format!("<!-- eka-ci-change-summary-{} -->", job);
    let comment_body = format!("{}\n\n{}", marker, markdown);

    // Post as a new comment (Gitea doesn't have built-in sticky comments like GitLab)
    // In the future, we could search for existing comments and update them
    match client
        .create_issue_comment(
            &ci_info.owner,
            &ci_info.repo_name,
            pr.pr_number,
            &comment_body,
        )
        .await
    {
        Ok(_) => {
            info!(
                "Posted change-summary comment for PR #{} in {}/{}/{} (domain: {})",
                pr.pr_number, ci_info.domain, ci_info.owner, ci_info.repo_name, ci_info.domain
            );
        },
        Err(e) => {
            warn!(
                "Failed to post change-summary comment for PR #{}: {:?}",
                pr.pr_number, e
            );
        },
    }

    Ok(())
}
