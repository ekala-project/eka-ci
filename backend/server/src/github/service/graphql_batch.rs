//! Batched GraphQL mutations for check run updates.
//!
//! Instead of making one REST API call per check run update (which burns
//! through the 5,000 req/hr installation rate limit), this module
//! accumulates pending updates and flushes them as a single GraphQL
//! mutation with aliases. The GraphQL API has a **separate** 5,000
//! points/hr budget, and each batched request costs only 1 point
//! regardless of how many mutations are packed in.

use std::collections::HashMap;

use anyhow::{Context, Result};
use octocrab::Octocrab;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Maximum number of mutations per GraphQL request.
/// GitHub's GraphQL has complexity limits; 50 is safe.
const MAX_BATCH_SIZE: usize = 50;

/// A pending check run update to be flushed via GraphQL.
#[derive(Debug, Clone)]
struct PendingUpdate {
    repo_node_id: String,
    check_run_node_id: String,
    owner: String,
    status: &'static str,
    conclusion: Option<&'static str>,
    /// Optional build log tail for failed check runs.
    log_tail: Option<String>,
    /// Optional override for the output title. When set, used instead of
    /// the hardcoded "Build failed". Used by coalesced gates.
    output_title: Option<String>,
}

/// Accumulates check run updates and flushes them as batched GraphQL mutations.
pub struct CheckRunBatcher {
    /// Pending updates keyed by owner (for octocrab auth scoping).
    pending: Mutex<Vec<PendingUpdate>>,
}

impl CheckRunBatcher {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
        }
    }

    /// Queue a check run update for batched GraphQL flush.
    pub async fn queue_update(
        &self,
        owner: &str,
        repo_node_id: &str,
        check_run_node_id: &str,
        status: &'static str,
        conclusion: Option<&'static str>,
    ) {
        self.queue_update_with_log(owner, repo_node_id, check_run_node_id, status, conclusion, None)
            .await;
    }

    /// Queue a check run update with optional build log tail.
    pub async fn queue_update_with_log(
        &self,
        owner: &str,
        repo_node_id: &str,
        check_run_node_id: &str,
        status: &'static str,
        conclusion: Option<&'static str>,
        log_tail: Option<String>,
    ) {
        self.pending.lock().await.push(PendingUpdate {
            repo_node_id: repo_node_id.to_string(),
            check_run_node_id: check_run_node_id.to_string(),
            owner: owner.to_string(),
            status,
            conclusion,
            log_tail,
            output_title: None,
        });
    }

    /// Queue a check run update with a custom output title and summary.
    /// Used by coalesced gates to render variant status tables.
    pub async fn queue_update_with_output(
        &self,
        owner: &str,
        repo_node_id: &str,
        check_run_node_id: &str,
        status: &'static str,
        conclusion: Option<&'static str>,
        output_title: String,
        summary: String,
    ) {
        self.pending.lock().await.push(PendingUpdate {
            repo_node_id: repo_node_id.to_string(),
            check_run_node_id: check_run_node_id.to_string(),
            owner: owner.to_string(),
            status,
            conclusion,
            log_tail: Some(summary),
            output_title: Some(output_title),
        });
    }

    /// Drain all pending updates and return them grouped by owner.
    async fn drain(&self) -> HashMap<String, Vec<PendingUpdate>> {
        let updates = std::mem::take(&mut *self.pending.lock().await);
        let mut by_owner: HashMap<String, Vec<PendingUpdate>> = HashMap::new();
        for u in updates {
            by_owner.entry(u.owner.clone()).or_default().push(u);
        }
        by_owner
    }

    /// Flush all pending updates as batched GraphQL mutations.
    /// Returns (total_sent, total_failed).
    pub async fn flush<F>(&self, get_octocrab: F) -> (usize, usize)
    where
        F: Fn(&str) -> Result<Octocrab>,
    {
        let by_owner = self.drain().await;
        let mut total_sent = 0usize;
        let mut total_failed = 0usize;

        for (owner, updates) in by_owner {
            let octocrab = match get_octocrab(&owner) {
                Ok(o) => o,
                Err(e) => {
                    warn!("No octocrab for owner {}: {:?}", owner, e);
                    total_failed += updates.len();
                    continue;
                },
            };

            // Process in chunks of MAX_BATCH_SIZE
            for chunk in updates.chunks(MAX_BATCH_SIZE) {
                match send_batch(&octocrab, chunk).await {
                    Ok(n) => {
                        total_sent += n;
                        total_failed += chunk.len() - n;
                    },
                    Err(e) => {
                        warn!(
                            "GraphQL batch flush failed for owner {} ({} updates): {:?}",
                            owner,
                            chunk.len(),
                            e
                        );
                        total_failed += chunk.len();
                    },
                }
            }
        }

        if total_sent > 0 || total_failed > 0 {
            info!(
                "GraphQL batch flush: {} sent, {} failed",
                total_sent, total_failed
            );
        }

        (total_sent, total_failed)
    }

    /// Returns the number of pending updates.
    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }
}

/// Build and send a batched GraphQL mutation for a chunk of updates.
/// Returns the number of successful mutations.
async fn send_batch(octocrab: &Octocrab, updates: &[PendingUpdate]) -> Result<usize> {
    if updates.is_empty() {
        return Ok(0);
    }

    // Build the GraphQL mutation with aliases
    let mut mutations = Vec::new();
    for (i, u) in updates.iter().enumerate() {
        let conclusion_field = match u.conclusion {
            Some(c) => format!(", conclusion: {}", c),
            None => String::new(),
        };
        // For failures with a log tail or coalesced gates with summary,
        // include output in the mutation.
        let output_field = match &u.log_tail {
            Some(log) => {
                // Escape for GraphQL string literal
                let escaped = log
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n");
                let title = match &u.output_title {
                    Some(t) => {
                        let t_escaped = t
                            .replace('\\', "\\\\")
                            .replace('"', "\\\"")
                            .replace('\n', "\\n");
                        t_escaped
                    },
                    None => "Build failed".to_string(),
                };
                // Coalesced gates pass markdown directly; failure logs
                // are wrapped in a code block.
                let summary = if u.output_title.is_some() {
                    escaped
                } else {
                    format!("```\\n{}\\n```", escaped)
                };
                format!(
                    r#", output: {{title: "{}", summary: "{}"}}"#,
                    title, summary,
                )
            },
            None => String::new(),
        };
        mutations.push(format!(
            r#"u{i}: updateCheckRun(input: {{repositoryId: "{repo}", checkRunId: "{cr}", status: {status}{conclusion}{output}}}) {{ checkRun {{ databaseId }} }}"#,
            i = i,
            repo = u.repo_node_id,
            cr = u.check_run_node_id,
            status = u.status,
            conclusion = conclusion_field,
            output = output_field,
        ));
    }

    let query = format!("mutation {{\n  {}\n}}", mutations.join("\n  "));

    debug!(
        "Sending GraphQL batch with {} mutations",
        updates.len()
    );

    let payload = serde_json::json!({ "query": query });
    let response: serde_json::Value = octocrab
        .graphql(&payload)
        .await
        .context("GraphQL batch mutation failed")?;

    // Count successes by checking for non-null data entries
    let mut success_count = 0;
    if let Some(data) = response.get("data") {
        for i in 0..updates.len() {
            let key = format!("u{}", i);
            if let Some(entry) = data.get(&key) {
                if entry.get("checkRun").is_some() {
                    success_count += 1;
                }
            }
        }
    }

    // Log any errors
    if let Some(errors) = response.get("errors") {
        if let Some(arr) = errors.as_array() {
            for err in arr {
                warn!(
                    "GraphQL mutation error: {}",
                    err.get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown")
                );
            }
        }
    }

    Ok(success_count)
}

/// Map DrvBuildState to GraphQL status/conclusion strings.
/// Returns (status, conclusion) where conclusion is None for non-terminal states.
pub fn build_state_to_graphql(
    state: &crate::db::model::build_event::DrvBuildState,
) -> (&'static str, Option<&'static str>) {
    use crate::db::model::build_event::{DrvBuildInterruptionKind, DrvBuildResult, DrvBuildState};

    match state {
        DrvBuildState::Queued | DrvBuildState::Buildable => ("QUEUED", None),
        DrvBuildState::FailedRetry | DrvBuildState::Building => ("IN_PROGRESS", None),
        DrvBuildState::Completed(DrvBuildResult::Success) => ("COMPLETED", Some("SUCCESS")),
        DrvBuildState::Completed(DrvBuildResult::Failure) => ("COMPLETED", Some("FAILURE")),
        DrvBuildState::TransitiveFailure => ("COMPLETED", Some("FAILURE")),
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Timeout) => {
            ("COMPLETED", Some("TIMED_OUT"))
        },
        DrvBuildState::Interrupted(DrvBuildInterruptionKind::Cancelled) => {
            ("COMPLETED", Some("NEUTRAL"))
        },
        DrvBuildState::Interrupted(_) => ("COMPLETED", Some("FAILURE")),
        DrvBuildState::Blocked => ("COMPLETED", Some("FAILURE")),
        DrvBuildState::UnsatisfiableRequirements => ("COMPLETED", Some("FAILURE")),
    }
}
