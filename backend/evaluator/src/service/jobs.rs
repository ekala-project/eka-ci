use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use sandbox::eval::EvalSandbox;
use sandbox::{SandboxExit, SandboxedChild};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tracing::{debug, warn};

use crate::types::nix_eval_jobs::{NixEvalDrv, NixEvalError, NixEvalItem};

// This file is meant to handle the evaluation of a "job" which is similar
// to the "jobset" by hydra, in particular:
// - You pass the file path of a nix file
// - You can optionally pass arguments to the file, which should be structured as a function which
//   receives an attrset of inputs
// - The file outputs an [deeply nested] attrset of attrset<attr_path, drv>
//
// M4: the output consumer bounds every growth axis so an adversarial
// or accidentally-huge flake cannot OOM the server:
//   - `NIX_EVAL_JOBS_MAX_ENTRIES` caps total parsed items (drvs + errors).
//   - `NIX_EVAL_JOBS_MAX_STDOUT_BYTES` caps total bytes read from nix-eval-jobs stdout.
//   - `NIX_EVAL_JOBS_MAX_LINE_BYTES` caps the length of any single JSONL line (prevents a
//     newline-less adversarial stream from growing the line buffer without bound).
//
// On any cap hit, the child is killed and reaped, the caller receives
// an error, and a `NixEvalMetrics::truncated_total` counter is
// incremented with the trigger reason.

/// Maximum number of parsed output entries (drvs + errors combined)
/// accepted from a single nix-eval-jobs invocation.
pub const NIX_EVAL_JOBS_MAX_ENTRIES: usize = 25_000;

/// Maximum total bytes accepted from nix-eval-jobs stdout per
/// invocation (128 MiB — well beyond any legitimate flake, still
/// bounded enough to prevent OOM).
pub const NIX_EVAL_JOBS_MAX_STDOUT_BYTES: u64 = 128 * 1024 * 1024;

/// Maximum size of any single JSONL line emitted by nix-eval-jobs
/// (1 MiB — a single `NixEvalDrv` JSON encoding rarely exceeds a few
/// kilobytes).
pub const NIX_EVAL_JOBS_MAX_LINE_BYTES: usize = 1024 * 1024;

/// Reason the output consumer stopped early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Truncation {
    /// No cap hit; consumer ran to EOF cleanly.
    None,
    /// Entry count (drvs + errors) hit `max_entries`.
    MaxEntries,
    /// Cumulative stdout byte count hit `max_bytes`.
    MaxBytes,
    /// A single line exceeded `max_line_bytes` without a newline.
    MaxLineBytes,
}

impl Truncation {
    /// Prometheus label for the truncated_total counter.
    pub fn label(self) -> &'static str {
        match self {
            Truncation::None => "none",
            Truncation::MaxEntries => "max_entries",
            Truncation::MaxBytes => "max_bytes",
            Truncation::MaxLineBytes => "max_line_bytes",
        }
    }
}

/// Result of consuming a nix-eval-jobs stdout stream with explicit
/// resource caps.
pub struct ConsumeOutcome {
    pub jobs: Vec<NixEvalDrv>,
    pub errors: Vec<NixEvalError>,
    pub bytes_read: u64,
    pub truncation: Truncation,
}

/// Consume nix-eval-jobs JSONL output with explicit resource caps.
///
/// Parses one line at a time, enforcing:
/// - total entries (drvs + errors) <= `max_entries`
/// - total bytes read <= `max_bytes`
/// - any single line <= `max_line_bytes`
///
/// Malformed JSON lines are logged and skipped (consistent with the
/// historical behaviour); non-UTF-8 lines are logged and skipped.
/// The function never panics; it stops at the first cap hit and
/// returns whatever was successfully parsed so callers can log
/// partial context.
pub async fn process_nix_eval_output<R: tokio::io::AsyncBufRead + Unpin>(
    mut reader: R,
    max_entries: usize,
    max_bytes: u64,
    max_line_bytes: usize,
) -> ConsumeOutcome {
    let mut jobs: Vec<NixEvalDrv> = Vec::new();
    let mut errors: Vec<NixEvalError> = Vec::new();
    let mut bytes_read: u64 = 0;
    // Reused buffer to avoid per-line allocation.
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    // `max_line_bytes + 1` so a line at exactly the cap still sees the
    // terminating newline; anything beyond that is a `MaxLineBytes`
    // hit regardless of whether a newline appears.
    let per_line_take: u64 = max_line_bytes as u64 + 1;

    loop {
        buf.clear();
        let n = {
            let mut limited = (&mut reader).take(per_line_take);
            match limited.read_until(b'\n', &mut buf).await {
                Ok(n) => n,
                Err(e) => {
                    warn!(error = %e, "Error reading nix-eval-jobs stdout");
                    break;
                },
            }
        };

        // Detect the "no newline within per_line_take bytes" case.
        // `read_until` stops when it either sees `\n` or exhausts the
        // underlying reader. If we read `per_line_take` bytes with no
        // newline, the attacker is trying to force an unbounded buffer.
        if n == 0 {
            break; // EOF
        }

        bytes_read = bytes_read.saturating_add(n as u64);
        if bytes_read > max_bytes {
            return ConsumeOutcome {
                jobs,
                errors,
                bytes_read,
                truncation: Truncation::MaxBytes,
            };
        }

        let has_newline = buf.last() == Some(&b'\n');
        if !has_newline && buf.len() > max_line_bytes {
            // Filled the take-limited reader without seeing a newline.
            return ConsumeOutcome {
                jobs,
                errors,
                bytes_read,
                truncation: Truncation::MaxLineBytes,
            };
        }
        if has_newline {
            buf.pop(); // trim trailing '\n'
            if buf.last() == Some(&b'\r') {
                buf.pop(); // trim '\r' too (defensive — nix-eval-jobs is LF)
            }
        }
        if buf.is_empty() {
            continue;
        }

        let line = match std::str::from_utf8(&buf) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "Non-UTF-8 nix-eval-jobs line discarded");
                continue;
            },
        };

        match serde_json::from_str::<NixEvalItem>(line) {
            Err(e) => {
                warn!(
                    "Encountered error when deserializing nix-eval-jobs output: {:?}",
                    e
                );
                continue;
            },
            Ok(NixEvalItem::Drv(drv)) => jobs.push(drv),
            Ok(NixEvalItem::Error(err)) => {
                debug!("Collected evaluation error: {:?}", err);
                errors.push(err);
            },
        }

        if jobs.len() + errors.len() >= max_entries {
            return ConsumeOutcome {
                jobs,
                errors,
                bytes_read,
                truncation: Truncation::MaxEntries,
            };
        }
    }

    ConsumeOutcome {
        jobs,
        errors,
        bytes_read,
        truncation: Truncation::None,
    }
}

/// Run nix-eval-jobs and process output with resource caps.
///
/// This function spawns nix-eval-jobs, consumes its output with the
/// standard resource caps, and returns the parsed derivations and errors.
/// If a resource cap is hit, the child process is killed and an error is
/// returned.
///
/// The caller must provide:
/// - `file_path`: Path to the Nix file to evaluate
/// - `metrics`: Optional metrics collector for observability
/// - `on_drv_traversed`: Optional callback invoked for each drv with its input_drvs
pub async fn run_nix_eval_jobs<F>(
    sandbox: &EvalSandbox,
    file_path: &str,
    extra_roots: &[PathBuf],
    metrics: Option<&dyn crate::traits::EvalMetricsCollector>,
    mut on_drv_traversed: Option<F>,
) -> anyhow::Result<(Vec<NixEvalDrv>, Vec<NixEvalError>)>
where
    F: FnMut(
        &str,
        &Option<std::collections::HashMap<String, Vec<String>>>,
    ) -> Result<(), anyhow::Error>,
{
    let mut child = sandbox
        .spawn_nix_eval_jobs(Path::new(file_path), extra_roots)
        .await?;
    let (outcome, stderr_output) = consume_child_output(&mut child).await?;

    if outcome.truncation != Truncation::None {
        return Err(fail_truncated(&mut child, &outcome, metrics).await);
    }
    finish_clean(&mut child, &outcome, &stderr_output).await?;

    // Observability for the clean path.
    if let Some(m) = metrics {
        m.items_total_inc("drv", outcome.jobs.len() as u64);
        m.items_total_inc("error", outcome.errors.len() as u64);
        m.output_entries_observe((outcome.jobs.len() + outcome.errors.len()) as f64);
        m.output_bytes_observe(outcome.bytes_read as f64);
    }

    // Traverse after full parse. We accept the slight delay
    // relative to the pre-M4 streaming model: the caps above
    // already bound the traversal workload, and batching keeps
    // schema failures from leaking half-traversed state.
    if let Some(ref mut callback) = on_drv_traversed {
        for drv in &outcome.jobs {
            if let Err(e) = callback(&drv.drv_path, &drv.input_drvs) {
                warn!("Issue while traversing {} drv: {:?}", &drv.drv_path, e);
            }
        }
    }

    Ok((outcome.jobs, outcome.errors))
}

async fn consume_child_output(
    child: &mut SandboxedChild,
) -> anyhow::Result<(ConsumeOutcome, String)> {
    let stderr_handle = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut buf = String::new();
            if let Err(e) = stderr.read_to_string(&mut buf).await {
                debug!("reading nix-eval-jobs stderr failed: {e}");
            }
            buf
        })
    });

    let stdout = child
        .stdout
        .take()
        .context("nix-eval-jobs stdout was not captured")?;
    let outcome = process_nix_eval_output(
        BufReader::new(stdout),
        NIX_EVAL_JOBS_MAX_ENTRIES,
        NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
        NIX_EVAL_JOBS_MAX_LINE_BYTES,
    )
    .await;
    if outcome.truncation != Truncation::None {
        child.kill();
    }

    let stderr_output = match stderr_handle {
        Some(handle) => handle.await.unwrap_or_else(|e| {
            warn!("nix-eval-jobs stderr drain task failed: {e}");
            String::new()
        }),
        None => String::new(),
    };
    if !stderr_output.is_empty() {
        debug!("nix-eval-jobs stderr: {}", stderr_output.trim());
    }
    Ok((outcome, stderr_output))
}

async fn fail_truncated(
    child: &mut SandboxedChild,
    outcome: &ConsumeOutcome,
    metrics: Option<&dyn crate::traits::EvalMetricsCollector>,
) -> anyhow::Error {
    child.kill();
    if let Err(e) = child.wait().await {
        warn!("nix-eval-jobs child wait failed: {:?}", e);
    }

    if let Some(m) = metrics {
        m.truncated_total_inc(outcome.truncation.label());
    }

    warn!(
        reason = outcome.truncation.label(),
        jobs = outcome.jobs.len(),
        errors = outcome.errors.len(),
        bytes_read = outcome.bytes_read,
        "nix-eval-jobs output truncated"
    );

    anyhow!(
        "nix-eval-jobs output exceeded resource cap ({}): jobs={}, errors={}, bytes={}",
        outcome.truncation.label(),
        outcome.jobs.len(),
        outcome.errors.len(),
        outcome.bytes_read,
    )
}

async fn finish_clean(
    child: &mut SandboxedChild,
    outcome: &ConsumeOutcome,
    stderr_output: &str,
) -> anyhow::Result<()> {
    check_exit(child.wait().await?, outcome, stderr_output)
}

fn check_exit(
    exit: SandboxExit,
    outcome: &ConsumeOutcome,
    stderr_output: &str,
) -> anyhow::Result<()> {
    let status = match exit {
        SandboxExit::TimedOut(limit) => bail!(
            "nix-eval-jobs exceeded the evaluation timeout of {}s and was killed",
            limit.as_secs()
        ),
        SandboxExit::Exited(status) => status,
    };
    if status.success() {
        return Ok(());
    }
    if outcome.jobs.is_empty() && outcome.errors.is_empty() {
        bail!(
            "nix-eval-jobs failed ({status}) without producing output: {}",
            stderr_tail(stderr_output)
        );
    }
    warn!("nix-eval-jobs exited with {status} after producing output");
    Ok(())
}

fn stderr_tail(stderr: &str) -> String {
    let lines: Vec<&str> = stderr.trim().lines().collect();
    lines[lines.len().saturating_sub(10)..].join("\n")
}

#[cfg(test)]
#[path = "jobs_tests.rs"]
mod tests;
