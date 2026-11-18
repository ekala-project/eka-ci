//! Size limits for GitHub check-run output fields.
//!
//! GitHub rejects a check run whose `output.summary` or `output.text` is
//! longer than 65,535 characters ("Only 65535 characters are allowed"), with
//! HTTP 422. A rejected create/update is retried by the task journal, so an
//! oversized body fails again on every replay. Every outgoing summary/text
//! goes through [`cap_output`] so a large payload is truncated, not rejected.

/// GitHub's maximum length of `output.summary` and `output.text`.
pub(crate) const GITHUB_OUTPUT_MAX_CHARS: usize = 65_535;

/// Lines kept from the start and from the end of an oversized output. The
/// first and last lines of a log are generally the most revealing.
const KEEP_LINES: usize = 100;

/// Byte budget for the kept content, leaving room for the omission marker
/// and a closing code fence. Bytes are an upper bound on characters, so
/// staying under it in bytes also stays under GitHub's character limit.
const BODY_BUDGET: usize = GITHUB_OUTPUT_MAX_CHARS - 1_024;

/// Truncate `output` to fit GitHub's check-run output limit.
///
/// Returns `output` unchanged if it fits. Otherwise keeps the first and
/// last [`KEEP_LINES`] lines with a marker in between; if those lines are
/// still too long (very long lines), keeps the first and last half of the
/// byte budget instead.
pub(crate) fn cap_output(output: String) -> String {
    if output.len() <= GITHUB_OUTPUT_MAX_CHARS {
        return output;
    }
    let lines: Vec<&str> = output.lines().collect();
    let capped = if lines.len() > 2 * KEEP_LINES {
        let head = lines[..KEEP_LINES].join("\n");
        let tail = lines[lines.len() - KEEP_LINES..].join("\n");
        let omitted = format!("{} lines", lines.len() - 2 * KEEP_LINES);
        join_head_tail(&head, &tail, &omitted)
    } else {
        None
    };
    capped.unwrap_or_else(|| cap_by_bytes(&output))
}

/// [`cap_output`] for a borrowed string.
pub(crate) fn cap_output_str(output: &str) -> String {
    cap_output(output.to_string())
}

/// Join head and tail with an omission marker, or `None` if over budget.
fn join_head_tail(head: &str, tail: &str, omitted: &str) -> Option<String> {
    if head.len() + tail.len() > BODY_BUDGET {
        return None;
    }
    Some(format!(
        "{head}\n\n… {omitted} omitted to fit GitHub's size limit …\n\n{tail}"
    ))
}

/// Keep the first and last half of the byte budget, on char boundaries.
fn cap_by_bytes(output: &str) -> String {
    let half = BODY_BUDGET / 2;
    let mut head_end = half;
    while !output.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = output.len() - half;
    while !output.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let omitted = format!("{} bytes", tail_start - head_end);
    let mut capped =
        join_head_tail(&output[..head_end], &output[tail_start..], &omitted).unwrap_or_default();
    // The cut can split a code fence; an unterminated one would swallow
    // everything after it.
    if capped.matches("```").count() % 2 == 1 {
        capped.push_str("\n```");
    }
    capped
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARKER: &str = "omitted to fit GitHub's size limit";

    #[test]
    fn short_input_is_unchanged() {
        let input = "# small markdown\n\nbody".to_string();
        assert_eq!(cap_output(input.clone()), input);
    }

    #[test]
    fn input_at_limit_is_unchanged() {
        let input = "a".repeat(GITHUB_OUTPUT_MAX_CHARS);
        assert_eq!(cap_output(input.clone()), input);
    }

    #[test]
    fn keeps_first_and_last_lines() {
        let big: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
        let out = cap_output(big);
        assert!(out.len() <= GITHUB_OUTPUT_MAX_CHARS);
        assert!(out.starts_with("line 0\n"));
        assert!(out.contains("\nline 99\n"));
        assert!(!out.contains("\nline 100\n"));
        assert!(!out.contains("\nline 19899\n"));
        assert!(out.contains("\nline 19900\n"));
        assert!(out.ends_with("line 19999"));
        assert!(out.contains(&format!("19800 lines {MARKER}")));
    }

    #[test]
    fn long_lines_fall_back_to_bytes() {
        let line = "x".repeat(1_000);
        let big = format!("{line}\n").repeat(300);
        let out = cap_output(big);
        assert!(out.len() <= GITHUB_OUTPUT_MAX_CHARS);
        assert!(out.contains(&format!("bytes {MARKER}")));
    }

    #[test]
    fn single_line_preserves_utf8_boundary() {
        let s = "🦀".repeat(GITHUB_OUTPUT_MAX_CHARS);
        let out = cap_output(s);
        assert!(out.len() <= GITHUB_OUTPUT_MAX_CHARS);
        assert!(out.starts_with('🦀') && out.ends_with('🦀'));
    }

    #[test]
    fn closes_code_fence_split_by_byte_cut() {
        let s = format!("```\n{}", "y".repeat(100_000));
        let out = cap_output(s);
        assert!(out.len() <= GITHUB_OUTPUT_MAX_CHARS);
        assert_eq!(out.matches("```").count() % 2, 0);
    }
}
