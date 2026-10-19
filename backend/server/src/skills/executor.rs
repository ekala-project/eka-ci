use std::path::Path;

use anyhow::{Context, Result};
use tokio::process::Command;

/// Generate the git diff between `base_sha` and `head_sha` in the
/// given checkout directory. The diff is the primary context fed to
/// the AI model during skill evaluation.
pub async fn generate_diff(checkout: &Path, base_sha: &str, head_sha: &str) -> Result<String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        Command::new("git")
            .current_dir(checkout)
            .args(["diff", base_sha, head_sha])
            .output(),
    )
    .await
    .context("git diff timed out after 60 seconds")?
    .context("failed to execute git diff")?;

    if !output.status.success() {
        anyhow::bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stdout).context("git diff output is not valid UTF-8")
}

/// Read the prompt file from the checkout directory. The file path is
/// relative to the repo root.
pub async fn read_prompt_file(checkout: &Path, prompt_file: &Path) -> Result<String> {
    let full_path = checkout.join(prompt_file);

    // Path safety: ensure the resolved path stays within the checkout.
    let canonical = full_path
        .canonicalize()
        .with_context(|| format!("prompt file not found: {}", prompt_file.display()))?;
    let checkout_canonical = checkout
        .canonicalize()
        .context("checkout directory does not exist")?;
    if !canonical.starts_with(&checkout_canonical) {
        anyhow::bail!(
            "prompt file {} escapes the checkout directory",
            prompt_file.display()
        );
    }

    tokio::fs::read_to_string(&canonical)
        .await
        .with_context(|| format!("failed to read prompt file {}", prompt_file.display()))
}

/// Assemble the user-facing content sent to the model: the diff plus
/// PR metadata.
pub fn assemble_user_content(
    diff: &str,
    pr_title: Option<&str>,
    pr_description: Option<&str>,
    pr_author: Option<&str>,
) -> String {
    let mut parts = Vec::with_capacity(4);

    if let Some(title) = pr_title {
        parts.push(format!("PR Title: {title}"));
    }
    if let Some(author) = pr_author {
        parts.push(format!("PR Author: {author}"));
    }
    if let Some(desc) = pr_description {
        if !desc.is_empty() {
            parts.push(format!("PR Description:\n{desc}"));
        }
    }

    parts.push(format!("Diff:\n```diff\n{diff}\n```"));
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_with_all_metadata() {
        let content = assemble_user_content(
            "+new line",
            Some("Fix bug"),
            Some("This fixes the thing"),
            Some("alice"),
        );
        assert!(content.contains("PR Title: Fix bug"));
        assert!(content.contains("PR Author: alice"));
        assert!(content.contains("PR Description:\nThis fixes the thing"));
        assert!(content.contains("```diff\n+new line\n```"));
    }

    #[test]
    fn assemble_with_no_metadata() {
        let content = assemble_user_content("+added", None, None, None);
        assert!(content.contains("```diff\n+added\n```"));
        assert!(!content.contains("PR Title"));
    }

    #[test]
    fn assemble_skips_empty_description() {
        let content = assemble_user_content("+x", Some("T"), Some(""), None);
        assert!(!content.contains("PR Description"));
    }
}
