// Change summary generation and posting for GitLab MRs

use std::sync::Arc;

use anyhow::Result;
use tracing::{debug, info, warn};

use crate::gitlab::GitLabClient;
use crate::gitlab::types::GitLabCIInfo;
use crate::graph::GraphServiceHandle;
use crate::metrics::ChangeSummaryMetrics;

/// Post (or update) change summary as an MR comment
pub(super) async fn handle_create_change_summary_comment(
    ci_info: &Arc<GitLabCIInfo>,
    job: &str,
    client: &GitLabClient,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
    graph_handle: &GraphServiceHandle,
    change_summary_metrics: Option<&Arc<ChangeSummaryMetrics>>,
) -> Result<()> {
    let Some(base_sha) = ci_info.base_commit.as_deref() else {
        debug!(
            "Skipping change-summary for {}: no base commit (not an MR head)",
            &ci_info.commit
        );
        return Ok(());
    };

    // Resolve head jobset ID from GitLabJobSets
    let head_jobset: Option<i64> = sqlx::query_scalar(
        "SELECT ROWID FROM GitLabJobSets WHERE sha = ? AND job = ? AND domain = ?",
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
        "SELECT ROWID FROM GitLabJobSets WHERE sha = ? AND job = ? AND domain = ?",
    )
    .bind(base_sha)
    .bind(job)
    .bind(&ci_info.domain)
    .fetch_optional(db_pool)
    .await?;

    // Create JobsetData for the head commit
    let jobset_data = crate::jobset_data::JobsetData::new(
        &ci_info.owner,
        &ci_info.repo_name,
        &ci_info.domain,
        &ci_info.commit,
        job,
        None,
    );

    // Resolve options from jobset data
    let (opts, status) = crate::change_summary::resolve_options_from_jobset_data(
        &jobset_data,
        change_summary_metrics.map(|m| m.as_ref()),
    )
    .await;

    // Build change summary using platform-agnostic function
    let summary = match crate::change_summary::build_change_summary_from_jobset_ids(
        db_pool,
        graph_handle,
        head_jobset_id,
        base_jobset_id,
        &jobset_data,
        base_sha,
        &opts,
        &status,
        change_summary_metrics.map(|m| m.as_ref()),
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

    // Look up the MR by head SHA to get the MR IID
    let mr =
        match crate::db::gitlab::get_mr_by_head_sha(&ci_info.commit, ci_info.project_id, db_pool)
            .await
        {
            Ok(Some(mr)) => mr,
            Ok(None) => {
                debug!(
                    "No MR found for commit {} in project {}; skipping change-summary comment",
                    &ci_info.commit, ci_info.project_id
                );
                return Ok(());
            },
            Err(e) => {
                warn!(
                    "Failed to look up MR for commit {}: {:?}",
                    &ci_info.commit, e
                );
                return Ok(());
            },
        };

    // Post or update the sticky change summary comment
    let marker = format!("<!-- eka-ci-change-summary-{} -->", job);
    match client
        .post_or_update_sticky_comment(ci_info.project_id, mr.mr_iid, &marker, &markdown)
        .await
    {
        Ok(_) => {
            info!(
                "Posted change-summary comment for MR !{} in {}/{} (project {})",
                mr.mr_iid, ci_info.owner, ci_info.repo_name, ci_info.project_id
            );
        },
        Err(e) => {
            warn!(
                "Failed to post change-summary comment for MR !{}: {:?}",
                mr.mr_iid, e
            );
        },
    }

    Ok(())
}
