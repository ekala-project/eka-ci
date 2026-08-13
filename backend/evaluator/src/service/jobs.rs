use std::process::Stdio;

use anyhow::{Context, bail};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, warn};

use crate::types::nix_eval_jobs::{NixEvalDrv, NixEvalError, NixEvalItem};

/// This file is meant to handle the evaluation of a "job" which is similar
/// to the "jobset" by hydra, in particular:
/// - You pass the file path of a nix file
/// - You can optionally pass arguments to the file, which should be structured as a function which
///   receives an attrset of inputs
/// - The file outputs an [deeply nested] attrset of attrset<attr_path, drv>
///
/// M4: the output consumer bounds every growth axis so an adversarial
/// or accidentally-huge flake cannot OOM the server:
///   - `NIX_EVAL_JOBS_MAX_ENTRIES` caps total parsed items (drvs + errors).
///   - `NIX_EVAL_JOBS_MAX_STDOUT_BYTES` caps total bytes read from nix-eval-jobs stdout.
///   - `NIX_EVAL_JOBS_MAX_LINE_BYTES` caps the length of any single JSONL line (prevents a
///     newline-less adversarial stream from growing the line buffer without bound).
/// On any cap hit, the child is killed and reaped, the caller receives
/// an error, and a `NixEvalMetrics::truncated_total` counter is
/// incremented with the trigger reason.

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
    file_path: &str,
    metrics: Option<&dyn crate::traits::EvalMetricsCollector>,
    mut on_drv_traversed: Option<F>,
) -> anyhow::Result<(Vec<NixEvalDrv>, Vec<NixEvalError>)>
where
    F: FnMut(
        &str,
        &Option<std::collections::HashMap<String, Vec<String>>>,
    ) -> Result<(), anyhow::Error>,
{
    let mut cmd = Command::new("nix-eval-jobs")
        .arg("--show-input-drvs")
        .arg("--meta")
        .arg(file_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()) // Capture stderr to prevent it from appearing in server logs
        .spawn()
        .context("failed to spawn nix-eval-jobs")?;

    // Drain stderr concurrently to prevent pipe deadlock: if the child
    // fills the OS pipe buffer (~64 KB) for stderr while we're blocked
    // reading stdout, both sides stall. Spawning a reader task keeps
    // the stderr pipe drained.
    let stderr_handle = cmd.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut buf = String::new();
            let _ = stderr.read_to_string(&mut buf).await;
            buf
        })
    });

    let outcome = {
        let stdout = cmd
            .stdout
            .take()
            .context("nix-eval-jobs stdout was not captured")?;
        let reader = BufReader::new(stdout);
        process_nix_eval_output(
            reader,
            NIX_EVAL_JOBS_MAX_ENTRIES,
            NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
            NIX_EVAL_JOBS_MAX_LINE_BYTES,
        )
        .await
    };

    // Collect stderr output from the background drain task.
    let stderr_output = match stderr_handle {
        Some(handle) => handle.await.unwrap_or_default(),
        None => String::new(),
    };
    if !stderr_output.is_empty() {
        debug!("nix-eval-jobs stderr: {}", stderr_output.trim());
    }

    if outcome.truncation != Truncation::None {
        if let Err(e) = cmd.kill().await {
            warn!(
                "nix-eval-jobs child kill failed (may already be dead): {:?}",
                e
            );
        }

        if let Err(e) = cmd.wait().await {
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

        bail!(
            "nix-eval-jobs output exceeded resource cap ({}): jobs={}, errors={}, bytes={}",
            outcome.truncation.label(),
            outcome.jobs.len(),
            outcome.errors.len(),
            outcome.bytes_read,
        );
    } else {
        // Reap the child on the clean path too.
        if let Err(e) = cmd.wait().await {
            warn!("nix-eval-jobs child wait failed on clean path: {:?}", e);
        }
    }

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

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    /// A minimal valid NixEvalDrv JSON body — keeps every test line
    /// short enough that per-line caps aren't an issue in other tests.
    fn drv_json(attr: &str) -> String {
        format!(
            r#"{{"attr":"{attr}","attrPath":["{attr}"],"drvPath":"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0-{attr}.drv","inputDrvs":{{}},"name":"{attr}","outputs":{{"out":"/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0-{attr}"}},"system":"x86_64-linux"}}"#
        )
    }

    fn error_json(attr: &str, msg: &str) -> String {
        format!(r#"{{"attr":"{attr}","attrPath":["{attr}"],"error":"{msg}"}}"#)
    }

    #[tokio::test]
    async fn empty_input_yields_no_entries_and_no_truncation() {
        let outcome = process_nix_eval_output(
            Cursor::new(Vec::<u8>::new()),
            NIX_EVAL_JOBS_MAX_ENTRIES,
            NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
            NIX_EVAL_JOBS_MAX_LINE_BYTES,
        )
        .await;
        assert_eq!(outcome.jobs.len(), 0);
        assert_eq!(outcome.errors.len(), 0);
        assert_eq!(outcome.truncation, Truncation::None);
        assert_eq!(outcome.bytes_read, 0);
    }

    #[tokio::test]
    async fn happy_path_parses_drv_and_error_lines() {
        let mut data = String::new();
        data.push_str(&drv_json("a"));
        data.push('\n');
        data.push_str(&error_json("b", "oops"));
        data.push('\n');
        data.push_str(&drv_json("c"));
        data.push('\n');

        let outcome = process_nix_eval_output(
            Cursor::new(data.as_bytes().to_vec()),
            NIX_EVAL_JOBS_MAX_ENTRIES,
            NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
            NIX_EVAL_JOBS_MAX_LINE_BYTES,
        )
        .await;
        assert_eq!(outcome.jobs.len(), 2);
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(outcome.truncation, Truncation::None);
        assert!(outcome.bytes_read > 0);
        assert_eq!(outcome.jobs[0].attr, "a");
        assert_eq!(outcome.jobs[1].attr, "c");
        assert_eq!(outcome.errors[0].attr, "b");
    }

    #[tokio::test]
    async fn malformed_json_lines_are_skipped_without_truncation() {
        let mut data = String::new();
        data.push_str(&drv_json("good"));
        data.push('\n');
        data.push_str("not-json-at-all\n");
        data.push_str("{\"half\": \n"); // broken JSON
        data.push_str(&drv_json("also-good"));
        data.push('\n');

        let outcome = process_nix_eval_output(
            Cursor::new(data.as_bytes().to_vec()),
            NIX_EVAL_JOBS_MAX_ENTRIES,
            NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
            NIX_EVAL_JOBS_MAX_LINE_BYTES,
        )
        .await;
        assert_eq!(
            outcome.jobs.len(),
            2,
            "malformed lines must not crash parse"
        );
        assert_eq!(outcome.errors.len(), 0);
        assert_eq!(outcome.truncation, Truncation::None);
    }

    #[tokio::test]
    async fn non_utf8_lines_are_skipped() {
        // A valid line, then a non-UTF-8 line, then another valid
        // line. Non-UTF-8 must be dropped, not terminate parsing.
        let mut data: Vec<u8> = Vec::new();
        data.extend_from_slice(drv_json("alpha").as_bytes());
        data.push(b'\n');
        // 0xFF is never valid UTF-8.
        data.extend_from_slice(&[0xFFu8, 0xFEu8, b'x', b'\n']);
        data.extend_from_slice(drv_json("omega").as_bytes());
        data.push(b'\n');

        let outcome = process_nix_eval_output(
            Cursor::new(data),
            NIX_EVAL_JOBS_MAX_ENTRIES,
            NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
            NIX_EVAL_JOBS_MAX_LINE_BYTES,
        )
        .await;
        assert_eq!(outcome.jobs.len(), 2);
        assert_eq!(outcome.truncation, Truncation::None);
    }

    #[tokio::test]
    async fn max_entries_cap_halts_consumption() {
        let mut data = String::new();
        // 5 drv lines — we'll cap at 3.
        for i in 0..5 {
            data.push_str(&drv_json(&format!("d{i}")));
            data.push('\n');
        }

        let outcome = process_nix_eval_output(
            Cursor::new(data.as_bytes().to_vec()),
            3,
            NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
            NIX_EVAL_JOBS_MAX_LINE_BYTES,
        )
        .await;
        assert_eq!(outcome.jobs.len(), 3);
        assert_eq!(outcome.truncation, Truncation::MaxEntries);
    }

    #[tokio::test]
    async fn max_bytes_cap_halts_consumption_before_parse_completes() {
        // Build roughly 200 KB of drv lines, cap at 50 KB.
        let mut data = String::new();
        for i in 0..5000u32 {
            data.push_str(&drv_json(&format!("d{i}")));
            data.push('\n');
            if data.len() > 200_000 {
                break;
            }
        }
        assert!(data.len() > 50_000, "test prerequisite");

        let outcome = process_nix_eval_output(
            Cursor::new(data.as_bytes().to_vec()),
            NIX_EVAL_JOBS_MAX_ENTRIES,
            50_000, // 50 KB byte cap
            NIX_EVAL_JOBS_MAX_LINE_BYTES,
        )
        .await;
        assert_eq!(outcome.truncation, Truncation::MaxBytes);
        assert!(outcome.bytes_read > 50_000);
    }

    #[tokio::test]
    async fn max_line_bytes_cap_stops_newline_less_flood() {
        // A single 4 KiB blob with no newline — cap at 1 KiB.
        let data = vec![b'x'; 4 * 1024];
        let outcome = process_nix_eval_output(
            Cursor::new(data),
            NIX_EVAL_JOBS_MAX_ENTRIES,
            NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
            1024,
        )
        .await;
        assert_eq!(outcome.truncation, Truncation::MaxLineBytes);
        assert_eq!(outcome.jobs.len(), 0);
        assert_eq!(outcome.errors.len(), 0);
    }

    #[tokio::test]
    async fn max_line_bytes_accepts_line_exactly_at_cap() {
        // A line of `max_line_bytes` ASCII bytes followed by '\n' must
        // parse (or be a malformed-JSON skip), not trigger
        // MaxLineBytes. We build a valid JSON line padded to close to
        // the cap.
        let base = drv_json("pad");
        let target_len = 512usize; // cap for the test
        assert!(base.len() < target_len);
        // Pad the `name` field (noop for parsing) to stretch length.
        let pad = target_len - base.len();
        let padded = drv_json(&"a".repeat(pad.saturating_add(1).max(1)));
        // `padded` may now slightly exceed target_len — that's fine,
        // we just need "line at cap".
        let line_len = padded.len();
        let mut data = padded.clone();
        data.push('\n');

        let outcome = process_nix_eval_output(
            Cursor::new(data.into_bytes()),
            NIX_EVAL_JOBS_MAX_ENTRIES,
            NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
            line_len, // cap equal to the line body length
        )
        .await;
        // The line fits at the cap, so we must not see MaxLineBytes.
        assert_ne!(outcome.truncation, Truncation::MaxLineBytes);
    }

    #[tokio::test]
    async fn truncation_labels_are_stable() {
        assert_eq!(Truncation::None.label(), "none");
        assert_eq!(Truncation::MaxEntries.label(), "max_entries");
        assert_eq!(Truncation::MaxBytes.label(), "max_bytes");
        assert_eq!(Truncation::MaxLineBytes.label(), "max_line_bytes");
    }
}
