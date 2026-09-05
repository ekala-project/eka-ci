// GitHub Pull Request operations

use anyhow::Result;
use sqlx::{Pool, Row, Sqlite};

use super::types::{PullRequest, PullRequestInfo, PullRequestWithStats};

/// Upsert a pull request record (insert or update if exists)
pub async fn upsert_pull_request(
    pr_number: i64,
    owner: &str,
    repo_name: &str,
    head_sha: &str,
    base_sha: &str,
    title: &str,
    author: &str,
    state: &str,
    created_at: &str,
    updated_at: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO GitHubPullRequests
            (pr_number, owner, repo_name, head_sha, base_sha, title, author, state, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(owner, repo_name, pr_number) DO UPDATE SET
            head_sha = excluded.head_sha,
            base_sha = excluded.base_sha,
            title = excluded.title,
            state = excluded.state,
            updated_at = excluded.updated_at
        "#,
    )
    .bind(pr_number)
    .bind(owner)
    .bind(repo_name)
    .bind(head_sha)
    .bind(base_sha)
    .bind(title)
    .bind(author)
    .bind(state)
    .bind(created_at)
    .bind(updated_at)
    .execute(pool)
    .await?;

    Ok(())
}

/// Upsert a merge queue build record
/// Uses pr_number=0 as a sentinel value for merge queue builds
pub async fn upsert_merge_queue_build(
    owner: &str,
    repo_name: &str,
    head_sha: &str,
    base_sha: &str,
    base_ref: &str,
    merge_group_head_sha: &str,
    created_at: &str,
    updated_at: &str,
    pool: &Pool<Sqlite>,
) -> Result<()> {
    let title = format!("Merge queue for {}", base_ref);

    sqlx::query(
        r#"
        INSERT INTO GitHubPullRequests
            (pr_number, owner, repo_name, head_sha, base_sha, title, author, state, created_at, updated_at, is_merge_queue, merge_group_head_sha)
        VALUES (0, ?, ?, ?, ?, ?, 'github-merge-queue', 'open', ?, ?, TRUE, ?)
        ON CONFLICT(owner, repo_name, pr_number) DO UPDATE SET
            head_sha = excluded.head_sha,
            base_sha = excluded.base_sha,
            title = excluded.title,
            updated_at = excluded.updated_at,
            merge_group_head_sha = excluded.merge_group_head_sha
        "#,
    )
    .bind(owner)
    .bind(repo_name)
    .bind(head_sha)
    .bind(base_sha)
    .bind(&title)
    .bind(created_at)
    .bind(updated_at)
    .bind(merge_group_head_sha)
    .execute(pool)
    .await?;

    Ok(())
}

/// Get all open pull requests with their build statistics
pub async fn list_open_pull_requests(pool: &Pool<Sqlite>) -> Result<Vec<PullRequestWithStats>> {
    // Query for PRs and their basic info
    let pr_rows = sqlx::query(
        r#"
        SELECT
            pr.pr_number,
            pr.owner,
            pr.repo_name,
            pr.head_sha,
            pr.base_sha,
            pr.title,
            pr.author,
            pr.state,
            pr.created_at,
            pr.updated_at,
            g.ROWID as jobset_id,
            COALESCE(COUNT(d.ROWID), 0) as total_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 42 THEN 1 ELSE 0 END), 0) as completed_success_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = -1 THEN 1 ELSE 0 END), 0) as completed_failure_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 2 THEN 1 ELSE 0 END), 0) as failed_retry_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 1 THEN 1 ELSE 0 END), 0) as changed_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 0 THEN 1 ELSE 0 END), 0) as new_drvs
        FROM GitHubPullRequests pr
        LEFT JOIN GitHubJobSets g ON pr.head_sha = g.sha
        LEFT JOIN Job j ON g.ROWID = j.jobset
        LEFT JOIN Drv d ON j.drv_id = d.ROWID
        WHERE pr.state = 'open'
        GROUP BY pr.pr_number, pr.owner, pr.repo_name
        ORDER BY pr.updated_at DESC
        "#,
    )
    .fetch_all(pool)
    .await?;

    let prs = pr_rows
        .into_iter()
        .map(|row| PullRequestWithStats {
            pr_info: PullRequestInfo {
                pr_number: row.get("pr_number"),
                owner: row.get("owner"),
                repo_name: row.get("repo_name"),
                head_sha: row.get("head_sha"),
                base_sha: row.get("base_sha"),
                title: row.get("title"),
                author: row.get("author"),
                state: row.get("state"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            },
            jobset_id: row.get("jobset_id"),
            total_drvs: row.get("total_drvs"),
            completed_success_drvs: row.get("completed_success_drvs"),
            completed_failure_drvs: row.get("completed_failure_drvs"),
            failed_retry_drvs: row.get("failed_retry_drvs"),
            changed_drvs: row.get("changed_drvs"),
            new_drvs: row.get("new_drvs"),
        })
        .collect();

    Ok(prs)
}

/// Get a specific pull request with build statistics
pub async fn get_pull_request(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<PullRequestWithStats>> {
    let pr_row = sqlx::query(
        r#"
        SELECT
            pr.pr_number,
            pr.owner,
            pr.repo_name,
            pr.head_sha,
            pr.base_sha,
            pr.title,
            pr.author,
            pr.state,
            pr.created_at,
            pr.updated_at,
            g.ROWID as jobset_id,
            COALESCE(COUNT(d.ROWID), 0) as total_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 42 THEN 1 ELSE 0 END), 0) as completed_success_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = -1 THEN 1 ELSE 0 END), 0) as completed_failure_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 2 THEN 1 ELSE 0 END), 0) as failed_retry_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 1 THEN 1 ELSE 0 END), 0) as changed_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 0 THEN 1 ELSE 0 END), 0) as new_drvs
        FROM GitHubPullRequests pr
        LEFT JOIN GitHubJobSets g ON pr.head_sha = g.sha
        LEFT JOIN Job j ON g.ROWID = j.jobset
        LEFT JOIN Drv d ON j.drv_id = d.ROWID
        WHERE pr.owner = ? AND pr.repo_name = ? AND pr.pr_number = ?
        GROUP BY pr.pr_number, pr.owner, pr.repo_name
        "#,
    )
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .fetch_optional(pool)
    .await?;

    Ok(pr_row.map(|row| PullRequestWithStats {
        pr_info: PullRequestInfo {
            pr_number: row.get("pr_number"),
            owner: row.get("owner"),
            repo_name: row.get("repo_name"),
            head_sha: row.get("head_sha"),
            base_sha: row.get("base_sha"),
            title: row.get("title"),
            author: row.get("author"),
            state: row.get("state"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        },
        jobset_id: row.get("jobset_id"),
        total_drvs: row.get("total_drvs"),
        completed_success_drvs: row.get("completed_success_drvs"),
        completed_failure_drvs: row.get("completed_failure_drvs"),
        failed_retry_drvs: row.get("failed_retry_drvs"),
        changed_drvs: row.get("changed_drvs"),
        new_drvs: row.get("new_drvs"),
    }))
}

/// Get all merge queue builds for a repository with their build statistics
pub async fn list_merge_queue_builds(
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<Vec<PullRequestWithStats>> {
    let pr_rows = sqlx::query(
        r#"
        SELECT
            pr.pr_number,
            pr.owner,
            pr.repo_name,
            pr.head_sha,
            pr.base_sha,
            pr.title,
            pr.author,
            pr.state,
            pr.created_at,
            pr.updated_at,
            g.ROWID as jobset_id,
            COALESCE(COUNT(d.ROWID), 0) as total_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 42 THEN 1 ELSE 0 END), 0) as completed_success_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = -1 THEN 1 ELSE 0 END), 0) as completed_failure_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 2 THEN 1 ELSE 0 END), 0) as failed_retry_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 1 THEN 1 ELSE 0 END), 0) as changed_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 0 THEN 1 ELSE 0 END), 0) as new_drvs
        FROM GitHubPullRequests pr
        LEFT JOIN GitHubJobSets g ON pr.head_sha = g.sha
        LEFT JOIN Job j ON g.ROWID = j.jobset
        LEFT JOIN Drv d ON j.drv_id = d.ROWID
        WHERE pr.owner = ? AND pr.repo_name = ? AND pr.is_merge_queue = TRUE
        GROUP BY pr.pr_number, pr.owner, pr.repo_name
        ORDER BY pr.updated_at DESC
        "#,
    )
    .bind(owner)
    .bind(repo_name)
    .fetch_all(pool)
    .await?;

    let builds = pr_rows
        .into_iter()
        .map(|row| PullRequestWithStats {
            pr_info: PullRequestInfo {
                pr_number: row.get("pr_number"),
                owner: row.get("owner"),
                repo_name: row.get("repo_name"),
                head_sha: row.get("head_sha"),
                base_sha: row.get("base_sha"),
                title: row.get("title"),
                author: row.get("author"),
                state: row.get("state"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            },
            jobset_id: row.get("jobset_id"),
            total_drvs: row.get("total_drvs"),
            completed_success_drvs: row.get("completed_success_drvs"),
            completed_failure_drvs: row.get("completed_failure_drvs"),
            failed_retry_drvs: row.get("failed_retry_drvs"),
            changed_drvs: row.get("changed_drvs"),
            new_drvs: row.get("new_drvs"),
        })
        .collect();

    Ok(builds)
}

/// Get a specific merge queue build by commit SHA
pub async fn get_merge_queue_build_by_sha(
    owner: &str,
    repo_name: &str,
    sha: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<PullRequestWithStats>> {
    let pr_row = sqlx::query(
        r#"
        SELECT
            pr.pr_number,
            pr.owner,
            pr.repo_name,
            pr.head_sha,
            pr.base_sha,
            pr.title,
            pr.author,
            pr.state,
            pr.created_at,
            pr.updated_at,
            g.ROWID as jobset_id,
            COALESCE(COUNT(d.ROWID), 0) as total_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 42 THEN 1 ELSE 0 END), 0) as completed_success_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = -1 THEN 1 ELSE 0 END), 0) as completed_failure_drvs,
            COALESCE(SUM(CASE WHEN d.build_state = 2 THEN 1 ELSE 0 END), 0) as failed_retry_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 1 THEN 1 ELSE 0 END), 0) as changed_drvs,
            COALESCE(SUM(CASE WHEN j.difference = 0 THEN 1 ELSE 0 END), 0) as new_drvs
        FROM GitHubPullRequests pr
        LEFT JOIN GitHubJobSets g ON pr.head_sha = g.sha
        LEFT JOIN Job j ON g.ROWID = j.jobset
        LEFT JOIN Drv d ON j.drv_id = d.ROWID
        WHERE pr.owner = ? AND pr.repo_name = ? AND pr.is_merge_queue = TRUE AND pr.head_sha = ?
        GROUP BY pr.pr_number, pr.owner, pr.repo_name
        "#,
    )
    .bind(owner)
    .bind(repo_name)
    .bind(sha)
    .fetch_optional(pool)
    .await?;

    Ok(pr_row.map(|row| PullRequestWithStats {
        pr_info: PullRequestInfo {
            pr_number: row.get("pr_number"),
            owner: row.get("owner"),
            repo_name: row.get("repo_name"),
            head_sha: row.get("head_sha"),
            base_sha: row.get("base_sha"),
            title: row.get("title"),
            author: row.get("author"),
            state: row.get("state"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        },
        jobset_id: row.get("jobset_id"),
        total_drvs: row.get("total_drvs"),
        completed_success_drvs: row.get("completed_success_drvs"),
        completed_failure_drvs: row.get("completed_failure_drvs"),
        failed_retry_drvs: row.get("failed_retry_drvs"),
        changed_drvs: row.get("changed_drvs"),
        new_drvs: row.get("new_drvs"),
    }))
}

/// Get PR by head SHA (for finding PR from jobset)
pub async fn get_pr_by_head_sha(
    sha: &str,
    owner: &str,
    repo_name: &str,
    pool: &Pool<Sqlite>,
) -> Result<Option<PullRequest>> {
    let pr = sqlx::query_as::<_, PullRequest>(
        "SELECT * FROM GitHubPullRequests
         WHERE head_sha = ? AND owner = ? AND repo_name = ?",
    )
    .bind(sha)
    .bind(owner)
    .bind(repo_name)
    .fetch_optional(pool)
    .await?;
    Ok(pr)
}

/// Fetch the raw `PullRequest` row (no stats). For callers needing
/// `comment_merge_*` / `auto_merge_enabled` / `head_sha`; use
/// `get_pull_request` for the stats-augmented variant.
pub async fn get_pull_request_row(
    owner: &str,
    repo_name: &str,
    pr_number: i64,
    pool: &Pool<Sqlite>,
) -> Result<Option<PullRequest>> {
    let pr = sqlx::query_as::<_, PullRequest>(
        "SELECT * FROM GitHubPullRequests
         WHERE owner = ? AND repo_name = ? AND pr_number = ?",
    )
    .bind(owner)
    .bind(repo_name)
    .bind(pr_number)
    .fetch_optional(pool)
    .await?;
    Ok(pr)
}
