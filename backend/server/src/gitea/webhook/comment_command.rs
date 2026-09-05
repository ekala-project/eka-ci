//! Comment command parsing for Gitea webhook comments.
//!
//! This module parses `@eka-ci` commands from pull request comments.

/// Comment command parsed from PR comments
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentCommand {
    /// `@eka-ci merge [method]` - Request merge
    Merge { method: Option<String> },
    /// `@eka-ci merge cancel` - Cancel pending merge
    MergeCancel,
}

/// Parse a comment body for `@eka-ci` commands
pub fn parse_comment_command(body: &str) -> Option<CommentCommand> {
    let body = body.trim();

    // Check if comment starts with @eka-ci mention
    if !body.starts_with("@eka-ci") {
        return None;
    }

    // Extract command after mention
    let after_mention = body.strip_prefix("@eka-ci")?.trim();

    if after_mention.is_empty() {
        return None;
    }

    // Parse merge commands
    if after_mention.starts_with("merge") {
        let after_merge = after_mention.strip_prefix("merge")?.trim();

        if after_merge.is_empty() {
            // Just "@eka-ci merge"
            return Some(CommentCommand::Merge { method: None });
        }

        if after_merge == "cancel" {
            return Some(CommentCommand::MergeCancel);
        }

        // "@eka-ci merge <method>"
        let method = after_merge.split_whitespace().next()?;
        return Some(CommentCommand::Merge {
            method: Some(method.to_string()),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_merge_simple() {
        assert_eq!(
            parse_comment_command("@eka-ci merge"),
            Some(CommentCommand::Merge { method: None })
        );
    }

    #[test]
    fn test_parse_merge_with_method() {
        assert_eq!(
            parse_comment_command("@eka-ci merge squash"),
            Some(CommentCommand::Merge {
                method: Some("squash".to_string())
            })
        );
    }

    #[test]
    fn test_parse_merge_cancel() {
        assert_eq!(
            parse_comment_command("@eka-ci merge cancel"),
            Some(CommentCommand::MergeCancel)
        );
    }

    #[test]
    fn test_parse_no_command() {
        assert_eq!(parse_comment_command("Just a regular comment"), None);
    }

    #[test]
    fn test_parse_mention_only() {
        assert_eq!(parse_comment_command("@eka-ci"), None);
    }
}
