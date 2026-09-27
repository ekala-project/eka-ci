use super::formatting::format_pr_summary;
use crate::db::DbService;

pub(super) async fn list_failing_prs(
    db: &DbService,
    repo_filter: Option<&str>,
) -> Result<String, String> {
    let prs = db
        .list_open_pull_requests()
        .await
        .map_err(|e| format!("Database error: {e}"))?;

    let failing: Vec<_> = prs
        .into_iter()
        .filter(|pr| pr.completed_failure_drvs > 0 || pr.failed_retry_drvs > 0)
        .filter(|pr| {
            repo_filter
                .map(|filter| {
                    let full_name =
                        format!("{}/{}", pr.pr_info.owner, pr.pr_info.repo_name).to_lowercase();
                    full_name.contains(&filter.to_lowercase())
                })
                .unwrap_or(true)
        })
        .collect();

    if failing.is_empty() {
        return Ok("No open PRs with failing gates found.".to_string());
    }

    let lines: Vec<String> = failing.iter().map(format_pr_summary).collect();
    Ok(format!(
        "Failing PRs ({}):\n{}",
        failing.len(),
        lines.join("\n")
    ))
}

pub(super) async fn get_pr_status(
    db: &DbService,
    owner: &str,
    repo: &str,
    pr_number: i64,
) -> Result<String, String> {
    let pr = db
        .get_pull_request(owner, repo, pr_number)
        .await
        .map_err(|e| format!("Database error: {e}"))?
        .ok_or_else(|| format!("PR #{pr_number} not found in {owner}/{repo}"))?;

    let mut output = format!(
        "PR #{} — {}\nRepository: {}/{}\nAuthor: {}\nHead SHA: {}\nState: {}\n",
        pr.pr_info.pr_number,
        pr.pr_info.title,
        pr.pr_info.owner,
        pr.pr_info.repo_name,
        pr.pr_info.author,
        pr.pr_info.head_sha,
        pr.pr_info.state,
    );

    // If there's a jobset, get the detailed breakdown
    if let Some(jobset_id) = pr.jobset_id {
        match db.get_jobset_details(jobset_id).await {
            Ok(details) => {
                output.push_str(&format_jobset_breakdown(&details));
            },
            Err(_) => {
                output.push_str(&format_pr_stats_fallback(&pr));
            },
        }
    } else {
        output.push_str("No jobset associated with this PR yet.\n");
    }

    output.push_str(&format!(
        "Changed packages: {}\nNew packages: {}",
        pr.changed_drvs, pr.new_drvs,
    ));

    Ok(output)
}

fn format_jobset_breakdown(details: &crate::db::github::JobSetDetails) -> String {
    format!(
        "Build status ({} total):\n\
         \x20 Success:              {}\n\
         \x20 Failed:               {}\n\
         \x20 Retrying:             {}\n\
         \x20 Transitive failure:   {}\n\
         \x20 Building:             {}\n\
         \x20 Buildable:            {}\n\
         \x20 Queued:               {}\n\
         \x20 Blocked:              {}\n\
         \x20 Interrupted:          {}\n",
        details.total_drvs,
        details.completed_success_drvs,
        details.completed_failure_drvs,
        details.failed_retry_drvs,
        details.transitive_failure_drvs,
        details.building_drvs,
        details.buildable_drvs,
        details.queued_drvs,
        details.blocked_drvs,
        details.interrupted_drvs,
    )
}

fn format_pr_stats_fallback(pr: &crate::db::github::PullRequestWithStats) -> String {
    use crate::db::github::PullRequestWithStats;
    let _: &PullRequestWithStats = pr; // type hint for readability
    format!(
        "Build status ({} total):\n\x20 Success: {}\n\x20 Failed:  {}\n\x20 Retry:   {}\n",
        pr.total_drvs, pr.completed_success_drvs, pr.completed_failure_drvs, pr.failed_retry_drvs,
    )
}
