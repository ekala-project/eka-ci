use anyhow::Result;
use sqlx::SqlitePool;

/// Get the baseline output size for a derivation on a specific branch
///
/// This looks up the most recent recorded output size for the given drv_path
/// on the base branch (e.g., "main"). Used to compare current PR builds against
/// the baseline to detect size increases.
///
/// # Arguments
/// * `pool` - Database connection pool
/// * `drv_path` - The derivation store path
/// * `git_repo` - Repository URL (e.g., "https://github.com/owner/repo")
/// * `base_branch` - Branch name to compare against (e.g., "main", "master")
///
/// # Returns
/// * `Ok(Some(size))` - Found baseline size in bytes
/// * `Ok(None)` - No baseline found (first time building this drv on base branch)
/// * `Err` - Database error
pub async fn get_baseline_output_size(
    pool: &SqlitePool,
    drv_path: &str,
    git_repo: &str,
    _base_branch: &str,
) -> Result<Option<u64>> {
    // We need to find commits on the base branch to get baseline sizes
    // Strategy: Get the most recent size measurement for this drv_path in this repo
    // that corresponds to a commit on the base branch
    //
    // Note: This requires that base branch has been built before.
    // For the first PR, there may be no baseline, which is OK - we just skip the check.

    let size = sqlx::query_scalar::<_, Option<i64>>(
        r#"
        SELECT s.output_size
        FROM DrvOutputSizes s
        INNER JOIN DrvBuildMetadata m ON s.drv_path = m.derivation AND s.git_commit = m.git_commit
        WHERE s.drv_path = ?
          AND s.git_repo = ?
        ORDER BY s.recorded_at DESC
        LIMIT 1
        "#,
    )
    .bind(drv_path)
    .bind(git_repo)
    .fetch_optional(pool)
    .await?;

    // Note: The query above doesn't filter by branch because git_commit is what matters
    // In practice, we're getting the most recent measurement. For a more precise implementation,
    // we'd need to:
    // 1. Query GitHub API or git to find commits on base_branch
    // 2. Filter to only those commits
    //
    // For now, we use "most recent successful build" as a reasonable baseline proxy.
    // TODO: Integrate with git/GitHub to verify commit is actually on base branch

    Ok(size.flatten().map(|s| s as u64))
}

/// Store the output size for a derivation build
///
/// Records the output size in the historical tracking table for future baseline comparisons.
///
/// # Arguments
/// * `pool` - Database connection pool
/// * `drv_path` - The derivation store path
/// * `output_size` - Size in bytes
/// * `git_commit` - Commit SHA this build corresponds to
/// * `git_repo` - Repository URL
pub async fn store_output_size(
    pool: &SqlitePool,
    drv_path: &str,
    output_size: u64,
    git_commit: &str,
    git_repo: &str,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO DrvOutputSizes (drv_path, output_size, git_commit, git_repo)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(drv_path, git_commit, git_repo) DO UPDATE SET
            output_size = excluded.output_size,
            recorded_at = CURRENT_TIMESTAMP
        "#,
    )
    .bind(drv_path)
    .bind(output_size as i64)
    .bind(git_commit)
    .bind(git_repo)
    .execute(pool)
    .await?;

    Ok(())
}

/// Update the output_size column in the Drv table
///
/// This is a convenience for updating the main Drv record with the calculated size.
///
/// # Arguments
/// * `pool` - Database connection pool
/// * `drv_path` - The derivation store path (DrvId)
/// * `output_size` - Size in bytes
pub async fn update_drv_output_size(
    pool: &SqlitePool,
    drv_path: &str,
    output_size: u64,
) -> Result<()> {
    sqlx::query(
        r#"
        UPDATE Drv
        SET output_size = ?
        WHERE drv_path = ?
        "#,
    )
    .bind(output_size as i64)
    .bind(drv_path)
    .execute(pool)
    .await?;

    Ok(())
}
