use anyhow::{Context, Result};
use colored::Colorize;
use serde::Deserialize;

pub struct ApiClient {
    client: reqwest::Client,
    base_url: String,
}

// --- Response types ---

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct ActiveBuilds {
    pub jobs: Vec<JobSetDetails>,
    pub building_drvs: Vec<BuildingDrv>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct JobSetDetails {
    pub jobset_id: i64,
    pub job_name: String,
    pub sha: String,
    pub owner: String,
    pub repo_name: String,
    pub total_drvs: i64,
    pub queued_drvs: i64,
    pub buildable_drvs: i64,
    pub building_drvs: i64,
    pub completed_success_drvs: i64,
    pub completed_failure_drvs: i64,
    pub failed_retry_drvs: i64,
    pub transitive_failure_drvs: i64,
    pub blocked_drvs: i64,
    pub interrupted_drvs: i64,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct BuildingDrv {
    pub drv_path: String,
    pub name: Option<String>,
    pub system: String,
    pub build_state: serde_json::Value,
    pub is_fod: bool,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct JobSetDrvsResponse {
    pub total: Option<i64>,
    pub drvs: Vec<JobSetDrv>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct JobSetDrv {
    pub drv_path: String,
    pub name: String,
    pub system: String,
    pub build_state: serde_json::Value,
    pub is_fod: bool,
    pub difference: serde_json::Value,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct DrvDependencies {
    pub dependencies: Vec<DrvDep>,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct DrvDep {
    pub drv_path: String,
    pub system: String,
    pub build_state: serde_json::Value,
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct PullRequestWithStats {
    pub pr_number: i64,
    pub owner: String,
    pub repo_name: String,
    pub head_sha: String,
    pub title: String,
    pub author: String,
    pub state: String,
    pub total_drvs: i64,
    pub completed_success_drvs: i64,
    pub completed_failure_drvs: i64,
    pub failed_retry_drvs: i64,
    pub changed_drvs: i64,
    pub new_drvs: i64,
    pub jobset_id: Option<i64>,
}

// --- Display helpers ---

fn format_state(state: &serde_json::Value) -> String {
    match state {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(map) => {
            if let Some(result) = map.get("Completed") {
                format!("Completed({})", result.as_str().unwrap_or("?"))
            } else if let Some(kind) = map.get("Interrupted") {
                format!("Interrupted({})", kind.as_str().unwrap_or("?"))
            } else {
                format!("{}", state)
            }
        },
        _ => format!("{}", state),
    }
}

fn color_state(state: &serde_json::Value) -> colored::ColoredString {
    let s = format_state(state);
    match s.as_str() {
        "Completed(Success)" => s.green(),
        "Completed(Failure)" | "TransitiveFailure" => s.red(),
        "FailedRetry" => s.yellow(),
        "Building" => s.cyan(),
        "Queued" | "Buildable" => s.blue(),
        _ if s.starts_with("Interrupted") => s.red(),
        _ => s.dimmed(),
    }
}

fn is_failure_state(state: &serde_json::Value) -> bool {
    let s = format_state(state);
    matches!(
        s.as_str(),
        "Completed(Failure)"
            | "TransitiveFailure"
            | "FailedRetry"
            | "UnsatisfiableRequirements"
    ) || s.starts_with("Interrupted")
}

fn drv_basename(drv: &str) -> &str {
    drv.rsplit_once('/').map_or(drv, |(_, b)| b)
}

fn short_name(drv_path: &str) -> &str {
    // Strip hash prefix: "hash-name.drv" -> "name.drv"
    let base = drv_basename(drv_path);
    if base.len() > 33 && base.as_bytes()[32] == b'-' {
        &base[33..]
    } else {
        base
    }
}

// --- ApiClient implementation ---

impl ApiClient {
    pub fn new(base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}/v1{}", self.base_url, path);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("failed to connect to EkaCI API at {}", self.base_url))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API returned {}: {}", status, body.trim());
        }

        response
            .json()
            .await
            .context("failed to parse API response")
    }

    async fn get_text(&self, path: &str) -> Result<String> {
        let url = format!("{}/v1{}", self.base_url, path);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("failed to connect to EkaCI API at {}", self.base_url))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API returned {}: {}", status, body.trim());
        }

        response.text().await.context("failed to read response")
    }

    // --- Command handlers ---

    pub async fn show_pr(&self, owner: &str, repo: &str, pr_number: i64) -> Result<()> {
        // Try the PR endpoint first; fall back to jobset listing if PR isn't tracked
        let pr_result: Result<PullRequestWithStats, _> = self
            .get_json(&format!("/prs/{}/{}/{}", owner, repo, pr_number))
            .await;

        match pr_result {
            Ok(pr) => {
                println!(
                    "{} #{} - {} (by {})",
                    "PR".bold(),
                    pr.pr_number,
                    pr.title,
                    pr.author.dimmed()
                );
                println!(
                    "  SHA: {}",
                    &pr.head_sha[..12.min(pr.head_sha.len())]
                );
                self.print_pr_stats(
                    pr.completed_success_drvs,
                    pr.completed_failure_drvs,
                    pr.failed_retry_drvs,
                    pr.total_drvs,
                );

                if let Some(jobset_id) = pr.jobset_id {
                    self.print_jobset_failures(jobset_id).await?;
                }
            },
            Err(_) => {
                // PR not in database — look up jobsets for this repo and show the latest
                println!(
                    "{} #{} (PR not tracked — showing repo jobsets)",
                    "PR".bold(),
                    pr_number,
                );
                self.list_jobs(Some(owner), Some(repo)).await?;
            },
        }

        Ok(())
    }

    fn print_pr_stats(&self, success: i64, failure: i64, retry: i64, total: i64) {
        println!(
            "  {} / {} passing",
            success.to_string().green(),
            total
        );
        if failure > 0 {
            println!("  {} failures", failure.to_string().red());
        }
        if retry > 0 {
            println!("  {} retrying", retry.to_string().yellow());
        }
        let pending = total - success - failure - retry;
        if pending > 0 {
            println!("  {} pending", pending.to_string().blue());
        }
    }

    async fn print_jobset_failures(&self, jobset_id: i64) -> Result<()> {
        let drvs: JobSetDrvsResponse = self
            .get_json(&format!("/jobs/{}/drvs?limit=10000", jobset_id))
            .await?;

        let failures: Vec<_> = drvs
            .drvs
            .iter()
            .filter(|d| is_failure_state(&d.build_state))
            .collect();

        if !failures.is_empty() {
            println!("\n{}", "Failures:".red().bold());
            for d in &failures {
                println!("  {} {}", color_state(&d.build_state), d.name.bold());
            }
        }

        Ok(())
    }

    pub async fn list_jobs(
        &self,
        owner: Option<&str>,
        repo: Option<&str>,
    ) -> Result<()> {
        let active: ActiveBuilds = self.get_json("/builds/active").await?;

        let jobs: Vec<&JobSetDetails> = if let (Some(o), Some(r)) = (owner, repo) {
            active
                .jobs
                .iter()
                .filter(|j| j.owner == o && j.repo_name == r)
                .collect()
        } else {
            active.jobs.iter().collect()
        };

        if jobs.is_empty() {
            println!("No active jobs");
            return Ok(());
        }

        println!(
            "{:<6} {:<20} {:<14} {:>6} {:>6} {:>6} {:>6} {:>6}",
            "ID".bold(),
            "Job".bold(),
            "SHA".bold(),
            "OK".green(),
            "Fail".red(),
            "Build".cyan(),
            "Queue".blue(),
            "Total".bold(),
        );

        for j in &jobs {
            println!(
                "{:<6} {:<20} {:<14} {:>6} {:>6} {:>6} {:>6} {:>6}",
                j.jobset_id,
                &j.job_name,
                &j.sha[..12.min(j.sha.len())],
                j.completed_success_drvs,
                j.completed_failure_drvs + j.transitive_failure_drvs + j.failed_retry_drvs,
                j.building_drvs,
                j.queued_drvs + j.buildable_drvs,
                j.total_drvs,
            );
        }

        println!(
            "\n{} drvs currently building",
            active.building_drvs.len().to_string().cyan()
        );

        Ok(())
    }

    pub async fn show_jobset(
        &self,
        jobset_id: i64,
        state_filter: Option<&str>,
        failures_only: bool,
    ) -> Result<()> {
        let details: JobSetDetails = self
            .get_json(&format!("/jobs/{}", jobset_id))
            .await?;

        println!(
            "{} {} ({}/{})",
            "Jobset".bold(),
            details.job_name,
            details.owner,
            details.repo_name,
        );
        println!(
            "  SHA: {}  |  Total: {}",
            &details.sha[..12.min(details.sha.len())],
            details.total_drvs
        );
        println!(
            "  {} success  {} failure  {} transitive  {} retry  {} building  {} queued  {} buildable  {} interrupted",
            details.completed_success_drvs.to_string().green(),
            details.completed_failure_drvs.to_string().red(),
            details.transitive_failure_drvs.to_string().red(),
            details.failed_retry_drvs.to_string().yellow(),
            details.building_drvs.to_string().cyan(),
            details.queued_drvs.to_string().blue(),
            details.buildable_drvs.to_string().blue(),
            details.interrupted_drvs.to_string().dimmed(),
        );

        // Fetch drvs
        let url = if let Some(state) = state_filter {
            format!("/jobs/{}/drvs?limit=10000&state={}", jobset_id, state)
        } else {
            format!("/jobs/{}/drvs?limit=10000", jobset_id)
        };
        let drvs: JobSetDrvsResponse = self.get_json(&url).await?;

        let filtered: Vec<&JobSetDrv> = if failures_only {
            drvs.drvs
                .iter()
                .filter(|d| is_failure_state(&d.build_state))
                .collect()
        } else {
            drvs.drvs.iter().collect()
        };

        if filtered.is_empty() && (failures_only || state_filter.is_some()) {
            println!("\n  No matching derivations");
        } else if !filtered.is_empty() {
            println!(
                "\n{} ({} shown):",
                "Derivations".bold(),
                filtered.len()
            );
            for d in &filtered {
                println!(
                    "  {:<16} {:<40} {}",
                    color_state(&d.build_state),
                    d.name,
                    drv_basename(&d.drv_path).dimmed(),
                );
            }
        }

        Ok(())
    }

    pub async fn show_drv_deps(&self, drv_path: &str) -> Result<()> {
        let drv = drv_basename(drv_path);
        let deps: DrvDependencies = self
            .get_json(&format!("/drvs/{}/dependencies", drv))
            .await?;

        println!(
            "{} {} ({} deps)",
            "Dependencies for".bold(),
            short_name(drv),
            deps.dependencies.len(),
        );

        let mut completed = 0;
        let mut blockers = Vec::new();

        for d in &deps.dependencies {
            let s = format_state(&d.build_state);
            if s == "Completed(Success)" {
                completed += 1;
            } else {
                blockers.push(d);
            }
        }

        println!(
            "  {} / {} completed",
            completed.to_string().green(),
            deps.dependencies.len()
        );

        if blockers.is_empty() {
            println!("  {} All dependencies satisfied", "✓".green());
        } else {
            println!(
                "\n{} ({}):",
                "Blockers".red().bold(),
                blockers.len()
            );
            for d in &blockers {
                println!(
                    "  {} {}",
                    color_state(&d.build_state),
                    short_name(&d.drv_path),
                );
            }
        }

        Ok(())
    }

    pub async fn show_log(&self, drv_path: &str) -> Result<()> {
        let drv = drv_basename(drv_path);
        let log = self.get_text(&format!("/logs/{}", drv)).await?;

        if log.is_empty() {
            println!("No build log available for {}", short_name(drv));
        } else {
            print!("{}", log);
        }

        Ok(())
    }
}
