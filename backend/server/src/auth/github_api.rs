//! GitHub API integration for checking repository permissions

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use reqwest::Client;
use tokio::sync::RwLock;

use crate::auth::types::{GitHubPermission, GitHubRepoPermissionResponse};

/// Cache entry for permission checks
#[derive(Debug, Clone)]
struct PermissionCacheEntry {
    permission: GitHubPermission,
    cached_at: Instant,
}

/// GitHub API client for checking permissions
pub struct GitHubApiClient {
    http_client: Client,
    cache: Arc<RwLock<HashMap<String, PermissionCacheEntry>>>,
    cache_ttl: Duration,
}

impl GitHubApiClient {
    /// Create a new GitHub API client with 10-minute cache TTL
    pub fn new() -> Self {
        Self {
            http_client: Client::new(),
            cache: Arc::new(RwLock::new(HashMap::new())),
            cache_ttl: Duration::from_secs(600), // 10 minutes
        }
    }

    /// Check if a user has permission on a repository
    ///
    /// # Arguments
    /// * `access_token` - GitHub access token for the user
    /// * `owner` - Repository owner (org or user)
    /// * `repo` - Repository name
    ///
    /// # Returns
    /// The user's permission level on the repository
    pub async fn check_repo_permission(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<GitHubPermission> {
        let cache_key = format!("{}:{}:{}", access_token, owner, repo);

        // Check cache first
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(&cache_key) {
                if entry.cached_at.elapsed() < self.cache_ttl {
                    tracing::debug!("Cache hit for permission check: {}/{}", owner, repo);
                    return Ok(entry.permission.clone());
                }
            }
        }

        // Cache miss or expired - fetch from GitHub API
        tracing::debug!("Fetching permission from GitHub API: {}/{}", owner, repo);
        let permission = self
            .fetch_repo_permission(access_token, owner, repo)
            .await?;

        // Update cache
        {
            let mut cache = self.cache.write().await;
            cache.insert(
                cache_key,
                PermissionCacheEntry {
                    permission: permission.clone(),
                    cached_at: Instant::now(),
                },
            );
        }

        Ok(permission)
    }

    /// Fetch permission from GitHub API (no caching)
    async fn fetch_repo_permission(
        &self,
        access_token: &str,
        owner: &str,
        repo: &str,
    ) -> Result<GitHubPermission> {
        let url = format!("https://api.github.com/repos/{}/{}", owner, repo);

        let response = self
            .http_client
            .get(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .header("User-Agent", "eka-ci")
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .map_err(|e| anyhow!("Failed to fetch repository info: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("GitHub API returned error {}: {}", status, body));
        }

        let repo_info: GitHubRepoPermissionResponse = response
            .json()
            .await
            .map_err(|e| anyhow!("Failed to parse GitHub API response: {}", e))?;

        // Determine permission level based on GitHub's response
        let permission = if repo_info.permissions.admin {
            GitHubPermission::Admin
        } else if repo_info.permissions.maintain.unwrap_or(false) {
            GitHubPermission::Maintain
        } else if repo_info.permissions.push {
            GitHubPermission::Write
        } else if repo_info.permissions.triage.unwrap_or(false) {
            GitHubPermission::Triage
        } else if repo_info.permissions.pull {
            GitHubPermission::Read
        } else {
            GitHubPermission::None
        };

        Ok(permission)
    }

    /// Clear the permission cache (useful for testing or manual refresh)
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
        tracing::debug!("Permission cache cleared");
    }

    /// Remove specific cache entry
    pub async fn invalidate_cache(&self, access_token: &str, owner: &str, repo: &str) {
        let cache_key = format!("{}:{}:{}", access_token, owner, repo);
        let mut cache = self.cache.write().await;
        cache.remove(&cache_key);
        tracing::debug!("Invalidated cache for {}/{}", owner, repo);
    }
}

impl Default for GitHubApiClient {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_permission_cache() {
        let client = GitHubApiClient::new();

        // Test cache clearing
        client.clear_cache().await;

        // Test cache invalidation
        client.invalidate_cache("token", "owner", "repo").await;
    }

    #[test]
    fn test_permission_levels() {
        assert!(GitHubPermission::Admin.is_triage_or_higher());
        assert!(GitHubPermission::Maintain.is_triage_or_higher());
        assert!(GitHubPermission::Write.is_triage_or_higher());
        assert!(GitHubPermission::Triage.is_triage_or_higher());
        assert!(!GitHubPermission::Read.is_triage_or_higher());
        assert!(!GitHubPermission::None.is_triage_or_higher());
    }
}
