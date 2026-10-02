use std::io::Cursor;

use super::*;

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
    data.push_str("{\"half\": \n");
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
    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(drv_json("alpha").as_bytes());
    data.push(b'\n');
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
        50_000,
        NIX_EVAL_JOBS_MAX_LINE_BYTES,
    )
    .await;
    assert_eq!(outcome.truncation, Truncation::MaxBytes);
    assert!(outcome.bytes_read > 50_000);
}

#[tokio::test]
async fn max_line_bytes_cap_stops_newline_less_flood() {
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
    let base = drv_json("pad");
    let target_len = 512usize;
    assert!(base.len() < target_len);
    let pad = target_len - base.len();
    let padded = drv_json(&"a".repeat(pad.saturating_add(1).max(1)));
    let line_len = padded.len();
    let mut data = padded.clone();
    data.push('\n');

    let outcome = process_nix_eval_output(
        Cursor::new(data.into_bytes()),
        NIX_EVAL_JOBS_MAX_ENTRIES,
        NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
        line_len,
    )
    .await;
    assert_ne!(outcome.truncation, Truncation::MaxLineBytes);
}

#[tokio::test]
async fn truncation_labels_are_stable() {
    assert_eq!(Truncation::None.label(), "none");
    assert_eq!(Truncation::MaxEntries.label(), "max_entries");
    assert_eq!(Truncation::MaxBytes.label(), "max_bytes");
    assert_eq!(Truncation::MaxLineBytes.label(), "max_line_bytes");
}

fn outcome(jobs: usize) -> ConsumeOutcome {
    let data: String = (0..jobs)
        .map(|i| drv_json(&format!("j{i}")) + "\n")
        .collect();
    outcome_from(data)
}

fn outcome_from(data: String) -> ConsumeOutcome {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(process_nix_eval_output(
            Cursor::new(data.into_bytes()),
            NIX_EVAL_JOBS_MAX_ENTRIES,
            NIX_EVAL_JOBS_MAX_STDOUT_BYTES,
            NIX_EVAL_JOBS_MAX_LINE_BYTES,
        ))
}

fn exited(code: i32) -> SandboxExit {
    use std::os::unix::process::ExitStatusExt;
    SandboxExit::Exited(std::process::ExitStatus::from_raw(code << 8))
}

#[test]
fn timeout_reports_a_clear_error_even_with_output() {
    let limit = std::time::Duration::from_secs(1800);
    let err = check_exit(SandboxExit::TimedOut(limit), &outcome(1), "").unwrap_err();
    assert_eq!(
        err.to_string(),
        "nix-eval-jobs exceeded the evaluation timeout of 1800s and was killed"
    );
}

#[test]
fn failure_without_output_is_an_error_with_stderr_tail() {
    let err = check_exit(exited(1), &outcome(0), "error: syntax error").unwrap_err();
    assert!(err.to_string().contains("error: syntax error"), "{err}");
}

#[test]
fn failure_after_output_and_success_are_ok() {
    assert!(check_exit(exited(1), &outcome(1), "").is_ok());
    assert!(check_exit(exited(0), &outcome(0), "").is_ok());
}
