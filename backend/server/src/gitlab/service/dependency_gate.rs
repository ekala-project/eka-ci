// Dependency changes gate for GitLab

use anyhow::Result;
use tracing::debug;

use crate::gitlab::GitLabClient;
use crate::gitlab::types::GitLabCIInfo;

pub(super) async fn handle_create_dependency_changes_gate(
    ci_info: &GitLabCIInfo,
    jobset_id: i64,
    base_jobset_id: i64,
    client: &GitLabClient,
    db_pool: &sqlx::Pool<sqlx::Sqlite>,
) -> Result<()> {
    debug!(
        "Creating dependency changes gate for commit {} (jobset: {}, base: {})",
        &ci_info.commit, jobset_id, base_jobset_id
    );

    let comparisons = crate::dependency_comparison::compare_runtime_references_for_jobset(
        base_jobset_id,
        jobset_id,
        db_pool,
    )
    .await?;

    let dependency_diff =
        crate::dependency_comparison::format_dependency_changes_as_diff(&comparisons);

    crate::gitlab::actions::create_dependency_changes_gate(
        client,
        ci_info,
        &dependency_diff,
        comparisons.len(),
    )
    .await?;

    debug!(
        "Successfully created dependency changes gate with {} packages affected",
        comparisons.len()
    );
    Ok(())
}
