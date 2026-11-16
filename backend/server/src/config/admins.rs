//! Operator-declared admin users (`[[admins]]` in `ekaci.toml`).
//!
//! Admin status and the PR-approval allow-list otherwise live only in the
//! SQLite database, so a fresh database has no admin able to approve
//! anyone. Declaring admins in config makes them durable: on startup they
//! are added to `ApprovedUsers` and promoted in `AuthenticatedUsers`, and
//! on web login they are granted admin.

use std::collections::HashSet;

use anyhow::bail;
use serde::{Deserialize, Serialize};

/// A GitHub user declared as an admin in `ekaci.toml`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct AdminUserConfig {
    /// GitHub login (e.g. "octocat").
    pub username: String,
    /// Numeric GitHub user id. This is the identity that is matched on
    /// login, since usernames can change. Look it up with
    /// `gh api users/<username> -q .id`.
    pub github_id: i64,
}

/// Reject empty usernames, non-positive ids, and duplicate ids/usernames.
pub(crate) fn validate_admins(
    raw: Option<Vec<AdminUserConfig>>,
) -> anyhow::Result<Vec<AdminUserConfig>> {
    let admins = raw.unwrap_or_default();
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for admin in &admins {
        if admin.username.trim().is_empty() {
            bail!("admins: username must not be empty");
        }
        if admin.github_id <= 0 {
            bail!(
                "admins: github_id for '{}' must be a positive GitHub user id",
                admin.username
            );
        }
        if !ids.insert(admin.github_id) {
            bail!("admins: duplicate github_id {}", admin.github_id);
        }
        if !names.insert(admin.username.to_lowercase()) {
            bail!("admins: duplicate username '{}'", admin.username);
        }
    }
    Ok(admins)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admin(username: &str, github_id: i64) -> AdminUserConfig {
        AdminUserConfig {
            username: username.to_string(),
            github_id,
        }
    }

    #[test]
    fn missing_section_is_empty() {
        assert!(validate_admins(None).unwrap().is_empty());
    }

    #[test]
    fn accepts_valid_admins() {
        let admins = vec![admin("alice", 1), admin("bob", 2)];
        assert_eq!(validate_admins(Some(admins.clone())).unwrap(), admins);
    }

    #[test]
    fn rejects_invalid_admins() {
        assert!(validate_admins(Some(vec![admin("", 1)])).is_err());
        assert!(validate_admins(Some(vec![admin("alice", 0)])).is_err());
        assert!(validate_admins(Some(vec![admin("alice", 1), admin("bob", 1)])).is_err());
        assert!(validate_admins(Some(vec![admin("alice", 1), admin("Alice", 2)])).is_err());
    }
}
