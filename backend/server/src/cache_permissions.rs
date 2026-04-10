use anyhow::{bail, Result};

use crate::config::CacheConfig;

/// Context for permission checking
pub struct PermissionContext {
    pub repo_owner: String,
    pub repo_name: String,
    pub branch: Option<String>,
}

impl PermissionContext {
    pub fn repo_full_name(&self) -> String {
        format!("{}/{}", self.repo_owner, self.repo_name)
    }
}

/// Check if a repository/branch is allowed to use a specific cache
pub fn check_cache_permission(
    cache: &CacheConfig,
    context: &PermissionContext,
) -> Result<()> {
    let permissions = &cache.permissions;

    // If allow_all is true, grant access
    if permissions.allow_all {
        return Ok(());
    }

    // Check repository allowlist
    if !permissions.allowed_repos.is_empty() {
        let repo_full_name = context.repo_full_name();
        if !permissions.allowed_repos.contains(&repo_full_name) {
            bail!(
                "Repository {} is not allowed to use cache {}",
                repo_full_name,
                cache.id
            );
        }
    }

    // Check branch patterns if specified
    if !permissions.allowed_branches.is_empty() {
        if let Some(branch) = &context.branch {
            let branch_allowed = permissions
                .allowed_branches
                .iter()
                .any(|pattern| matches_glob_pattern(branch, pattern));

            if !branch_allowed {
                bail!(
                    "Branch {} is not allowed to use cache {}",
                    branch,
                    cache.id
                );
            }
        } else {
            // No branch context provided but branch restrictions exist
            bail!(
                "Branch information required for cache {} but not provided",
                cache.id
            );
        }
    }

    Ok(())
}

/// Simple glob pattern matching for branch names
/// Supports '*' as wildcard (e.g., "main", "release/*", "*")
fn matches_glob_pattern(value: &str, pattern: &str) -> bool {
    // If pattern is just "*", match everything
    if pattern == "*" {
        return true;
    }

    // If pattern ends with "/*", match prefix
    if let Some(prefix) = pattern.strip_suffix("/*") {
        return value.starts_with(prefix);
    }

    // If pattern starts with "*/", match suffix
    if let Some(suffix) = pattern.strip_prefix("*/") {
        return value.ends_with(suffix);
    }

    // Otherwise, exact match
    value == pattern
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CacheConfig, CachePermissions, CacheType, CredentialSource};

    fn make_test_cache(permissions: CachePermissions) -> CacheConfig {
        CacheConfig {
            id: "test-cache".to_string(),
            cache_type: CacheType::NixCopy,
            destination: "s3://test".to_string(),
            credentials: CredentialSource::None,
            permissions,
        }
    }

    #[test]
    fn test_allow_all() {
        let cache = make_test_cache(CachePermissions {
            allow_all: true,
            allowed_repos: vec![],
            allowed_branches: vec![],
        });

        let context = PermissionContext {
            repo_owner: "someuser".to_string(),
            repo_name: "somerepo".to_string(),
            branch: Some("anybranch".to_string()),
        };

        assert!(check_cache_permission(&cache, &context).is_ok());
    }

    #[test]
    fn test_repo_allowlist_allowed() {
        let cache = make_test_cache(CachePermissions {
            allow_all: false,
            allowed_repos: vec!["myorg/myrepo".to_string()],
            allowed_branches: vec![],
        });

        let context = PermissionContext {
            repo_owner: "myorg".to_string(),
            repo_name: "myrepo".to_string(),
            branch: None,
        };

        assert!(check_cache_permission(&cache, &context).is_ok());
    }

    #[test]
    fn test_repo_allowlist_denied() {
        let cache = make_test_cache(CachePermissions {
            allow_all: false,
            allowed_repos: vec!["myorg/myrepo".to_string()],
            allowed_branches: vec![],
        });

        let context = PermissionContext {
            repo_owner: "otherorg".to_string(),
            repo_name: "otherrepo".to_string(),
            branch: None,
        };

        assert!(check_cache_permission(&cache, &context).is_err());
    }

    #[test]
    fn test_branch_pattern_exact_match() {
        let cache = make_test_cache(CachePermissions {
            allow_all: false,
            allowed_repos: vec!["myorg/myrepo".to_string()],
            allowed_branches: vec!["main".to_string()],
        });

        let context = PermissionContext {
            repo_owner: "myorg".to_string(),
            repo_name: "myrepo".to_string(),
            branch: Some("main".to_string()),
        };

        assert!(check_cache_permission(&cache, &context).is_ok());
    }

    #[test]
    fn test_branch_pattern_wildcard() {
        let cache = make_test_cache(CachePermissions {
            allow_all: false,
            allowed_repos: vec!["myorg/myrepo".to_string()],
            allowed_branches: vec!["release/*".to_string()],
        });

        let context = PermissionContext {
            repo_owner: "myorg".to_string(),
            repo_name: "myrepo".to_string(),
            branch: Some("release/v1.0".to_string()),
        };

        assert!(check_cache_permission(&cache, &context).is_ok());
    }

    #[test]
    fn test_branch_pattern_mismatch() {
        let cache = make_test_cache(CachePermissions {
            allow_all: false,
            allowed_repos: vec!["myorg/myrepo".to_string()],
            allowed_branches: vec!["main".to_string()],
        });

        let context = PermissionContext {
            repo_owner: "myorg".to_string(),
            repo_name: "myrepo".to_string(),
            branch: Some("develop".to_string()),
        };

        assert!(check_cache_permission(&cache, &context).is_err());
    }

    #[test]
    fn test_glob_patterns() {
        assert!(matches_glob_pattern("main", "main"));
        assert!(matches_glob_pattern("anything", "*"));
        assert!(matches_glob_pattern("release/v1.0", "release/*"));
        assert!(matches_glob_pattern("feature/foo", "feature/*"));
        assert!(!matches_glob_pattern("main", "release/*"));
        assert!(!matches_glob_pattern("feature/foo", "main"));
    }
}
