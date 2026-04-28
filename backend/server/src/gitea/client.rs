use anyhow::{Context, Result, bail};
use reqwest::{Client, header};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Gitea API client
///
/// Gitea uses a GitHub-compatible API, so many endpoints and types
/// are similar to GitHub's. However, there are differences in
/// authentication (no App model) and version-dependent features.
pub struct GiteaClient {
    base_url: String,
    token: String,
    http_client: Client,
    capabilities: GiteaCapabilities,
}

#[derive(Debug, Clone)]
struct GiteaCapabilities {
    /// Whether this Gitea instance supports the Check Runs API (v1.13+)
    supports_check_runs: bool,
    /// Gitea version string
    version: String,
}

impl GiteaClient {
    /// Create a new Gitea client and detect instance capabilities
    pub async fn new(domain: &str, token: String) -> Result<Self> {
        let base_url = if domain.starts_with("http://") || domain.starts_with("https://") {
            domain.to_string()
        } else {
            format!("https://{}", domain)
        };

        let http_client = Client::builder()
            .user_agent("eka-ci")
            .build()
            .context("Failed to build HTTP client")?;

        let mut client = Self {
            base_url,
            token,
            http_client,
            capabilities: GiteaCapabilities {
                supports_check_runs: false,
                version: "unknown".to_string(),
            },
        };

        // Detect version and set capabilities
        client.detect_capabilities().await?;

        Ok(client)
    }

    /// Detect Gitea version and set capability flags
    async fn detect_capabilities(&mut self) -> Result<()> {
        #[derive(Deserialize)]
        struct VersionResponse {
            version: String,
        }

        let url = format!("{}/api/v1/version", self.base_url);
        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .context("Failed to fetch Gitea version")?;

        if !response.status().is_success() {
            warn!(
                "Failed to detect Gitea version ({}), assuming old version without check runs",
                response.status()
            );
            return Ok(());
        }

        let version_info: VersionResponse = response
            .json()
            .await
            .context("Failed to parse version response")?;

        self.capabilities.version = version_info.version.clone();

        // Check Runs API was added in Gitea 1.13.0
        self.capabilities.supports_check_runs =
            self.version_supports_check_runs(&version_info.version);

        debug!(
            "Detected Gitea {} (check runs: {})",
            self.capabilities.version, self.capabilities.supports_check_runs
        );

        Ok(())
    }

    /// Check if a version string indicates Check Runs API support
    fn version_supports_check_runs(&self, version: &str) -> bool {
        // Parse version string like "1.21.0" or "1.21.0+dev"
        let version_parts: Vec<&str> = version.split(&['.', '+', '-'][..]).collect();

        if version_parts.len() < 2 {
            return false;
        }

        let major: u32 = version_parts[0].parse().unwrap_or(0);
        let minor: u32 = version_parts[1].parse().unwrap_or(0);

        // Check Runs API added in 1.13.0
        major > 1 || (major == 1 && minor >= 13)
    }

    /// Get whether this instance supports Check Runs API
    pub fn supports_check_runs(&self) -> bool {
        self.capabilities.supports_check_runs
    }

    /// Get the Gitea version
    pub fn version(&self) -> &str {
        &self.capabilities.version
    }

    /// Build authorization header
    fn auth_header(&self) -> header::HeaderValue {
        header::HeaderValue::from_str(&format!("token {}", self.token))
            .expect("Token should be valid header value")
    }
}

// ==================================================================
// Check Runs API (Gitea 1.13+)
// ==================================================================

#[derive(Debug, Serialize)]
pub struct CreateCheckRunRequest {
    pub name: String,
    pub head_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<CheckConclusion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<CheckOutput>,
}

#[derive(Debug, Serialize)]
pub struct UpdateCheckRunRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<CheckConclusion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<CheckOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Queued,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckConclusion {
    Success,
    Failure,
    Neutral,
    Cancelled,
    TimedOut,
    ActionRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckOutput {
    pub title: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CheckRun {
    pub id: i64,
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub conclusion: Option<String>,
}

impl GiteaClient {
    /// Create a check run
    pub async fn create_check_run(
        &self,
        owner: &str,
        repo: &str,
        request: CreateCheckRunRequest,
    ) -> Result<CheckRun> {
        if !self.capabilities.supports_check_runs {
            bail!(
                "Check Runs API not supported by this Gitea instance ({})",
                self.capabilities.version
            );
        }

        let url = format!(
            "{}/api/v1/repos/{}/{}/check-runs",
            self.base_url, owner, repo
        );

        let response = self
            .http_client
            .post(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .json(&request)
            .send()
            .await
            .context("Failed to create check run")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to create check run ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse check run response")
    }

    /// Update a check run
    pub async fn update_check_run(
        &self,
        owner: &str,
        repo: &str,
        check_run_id: i64,
        request: UpdateCheckRunRequest,
    ) -> Result<()> {
        if !self.capabilities.supports_check_runs {
            bail!("Check Runs API not supported by this Gitea instance");
        }

        let url = format!(
            "{}/api/v1/repos/{}/{}/check-runs/{}",
            self.base_url, owner, repo, check_run_id
        );

        let response = self
            .http_client
            .patch(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .json(&request)
            .send()
            .await
            .context("Failed to update check run")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to update check run ({}): {}", status, body);
        }

        Ok(())
    }
}

// ==================================================================
// Commit Status API (fallback for older Gitea versions)
// ==================================================================

#[derive(Debug, Serialize)]
pub struct CreateCommitStatusRequest {
    pub state: CommitStatusState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub context: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitStatusState {
    Pending,
    Success,
    Error,
    Failure,
}

#[derive(Debug, Deserialize)]
pub struct CommitStatus {
    pub id: i64,
    pub status: String,
    pub context: String,
    #[serde(default)]
    pub description: Option<String>,
}

impl GiteaClient {
    /// Create or update a commit status
    pub async fn create_commit_status(
        &self,
        owner: &str,
        repo: &str,
        sha: &str,
        request: CreateCommitStatusRequest,
    ) -> Result<CommitStatus> {
        let url = format!(
            "{}/api/v1/repos/{}/{}/statuses/{}",
            self.base_url, owner, repo, sha
        );

        let response = self
            .http_client
            .post(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .json(&request)
            .send()
            .await
            .context("Failed to create commit status")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to create commit status ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse commit status response")
    }

    /// Get commit details
    ///
    /// Gitea API: GET /repos/:owner/:repo/git/commits/:sha
    pub async fn get_commit(&self, owner: &str, repo: &str, sha: &str) -> Result<GiteaCommit> {
        let url = format!(
            "{}/api/v1/repos/{}/{}/git/commits/{}",
            self.base_url, owner, repo, sha
        );

        let response = self
            .http_client
            .get(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("Failed to get commit")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to get commit ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse commit response")
    }
}

#[derive(Debug, Deserialize)]
pub struct GiteaCommit {
    pub sha: String,
    pub commit: CommitDetails,
}

#[derive(Debug, Deserialize)]
pub struct CommitDetails {
    pub message: String,
    pub author: CommitAuthor,
    pub committer: CommitAuthor,
}

#[derive(Debug, Deserialize)]
pub struct CommitAuthor {
    pub name: String,
    pub email: String,
    pub date: String, // ISO 8601 format
}

// ==================================================================
// Pull Request Operations
// ==================================================================

#[derive(Debug, Serialize)]
pub struct MergePullRequestRequest {
    #[serde(rename = "Do")]
    pub merge_method: String, // "merge", "rebase", "squash"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_message_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_title_field: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PullRequest {
    pub id: i64,
    pub number: i64,
    pub state: String,
    pub mergeable: bool,
    pub merged: bool,
}

#[derive(Debug, Deserialize)]
pub struct Review {
    pub id: i64,
    pub user: User,
    pub state: String, // "APPROVED", "REQUEST_CHANGES", "COMMENT"
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub id: i64,
    pub login: String,
}

#[derive(Debug, Deserialize)]
pub struct Repository {
    pub id: i64,
    pub name: String,
    pub owner: User,
    pub default_branch: String,
    pub allow_merge_commits: bool,
    pub allow_rebase: bool,
    pub allow_rebase_explicit: bool,
    pub allow_squash_merge: bool,
}

#[derive(Debug, Deserialize)]
pub struct Permission {
    pub permission: String, // "admin", "write", "read", "none"
}

impl GiteaClient {
    /// Merge a pull request
    pub async fn merge_pull_request(
        &self,
        owner: &str,
        repo: &str,
        index: i64,
        request: MergePullRequestRequest,
    ) -> Result<()> {
        let url = format!(
            "{}/api/v1/repos/{}/{}/pulls/{}/merge",
            self.base_url, owner, repo, index
        );

        let response = self
            .http_client
            .post(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .json(&request)
            .send()
            .await
            .context("Failed to merge pull request")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to merge pull request ({}): {}", status, body);
        }

        Ok(())
    }

    /// List pull request reviews
    pub async fn list_reviews(&self, owner: &str, repo: &str, index: i64) -> Result<Vec<Review>> {
        let url = format!(
            "{}/api/v1/repos/{}/{}/pulls/{}/reviews",
            self.base_url, owner, repo, index
        );

        let response = self
            .http_client
            .get(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("Failed to list reviews")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to list reviews ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse reviews response")
    }

    /// Get repository settings
    pub async fn get_repository(&self, owner: &str, repo: &str) -> Result<Repository> {
        let url = format!("{}/api/v1/repos/{}/{}", self.base_url, owner, repo);

        let response = self
            .http_client
            .get(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("Failed to get repository")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to get repository ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse repository response")
    }

    /// Check user's permission level for a repository
    pub async fn check_user_permission(
        &self,
        owner: &str,
        repo: &str,
        username: &str,
    ) -> Result<Permission> {
        let url = format!(
            "{}/api/v1/repos/{}/{}/collaborators/{}/permission",
            self.base_url, owner, repo, username
        );

        let response = self
            .http_client
            .get(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("Failed to check permission")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to check permission ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse permission response")
    }
}

// ==================================================================
// Comments and Reactions
// ==================================================================

#[derive(Debug, Serialize)]
pub struct CreateCommentRequest {
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct Comment {
    pub id: i64,
    pub body: String,
    pub user: User,
}

impl GiteaClient {
    /// Create an issue comment (works for PRs too)
    pub async fn create_issue_comment(
        &self,
        owner: &str,
        repo: &str,
        index: i64,
        body: &str,
    ) -> Result<Comment> {
        let url = format!(
            "{}/api/v1/repos/{}/{}/issues/{}/comments",
            self.base_url, owner, repo, index
        );

        let request = CreateCommentRequest {
            body: body.to_string(),
        };

        let response = self
            .http_client
            .post(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .json(&request)
            .send()
            .await
            .context("Failed to create comment")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to create comment ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse comment response")
    }

    /// Add a reaction to a comment
    pub async fn add_reaction(
        &self,
        owner: &str,
        repo: &str,
        comment_id: i64,
        reaction: &str,
    ) -> Result<()> {
        let url = format!(
            "{}/api/v1/repos/{}/{}/issues/comments/{}/reactions",
            self.base_url, owner, repo, comment_id
        );

        #[derive(Serialize)]
        struct ReactionRequest {
            content: String,
        }

        let request = ReactionRequest {
            content: reaction.to_string(),
        };

        let response = self
            .http_client
            .post(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .json(&request)
            .send()
            .await
            .context("Failed to add reaction")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to add reaction ({}): {}", status, body);
        }

        Ok(())
    }

    /// Get the domain/base URL of this Gitea instance
    pub fn get_domain(&self) -> &str {
        // Strip https:// or http:// prefix if present
        self.base_url
            .strip_prefix("https://")
            .or_else(|| self.base_url.strip_prefix("http://"))
            .unwrap_or(&self.base_url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_supports_check_runs_below_threshold() {
        let client = GiteaClient {
            base_url: "https://test.com".to_string(),
            token: "test".to_string(),
            http_client: Client::new(),
            capabilities: GiteaCapabilities {
                supports_check_runs: false,
                version: "1.12.0".to_string(),
            },
        };

        assert!(!client.version_supports_check_runs("1.12.0"));
        assert!(!client.version_supports_check_runs("1.12.9"));
        assert!(!client.version_supports_check_runs("1.0.0"));
        assert!(!client.version_supports_check_runs("0.9.0"));
    }

    #[test]
    fn test_version_supports_check_runs_at_threshold() {
        let client = GiteaClient {
            base_url: "https://test.com".to_string(),
            token: "test".to_string(),
            http_client: Client::new(),
            capabilities: GiteaCapabilities {
                supports_check_runs: false,
                version: "1.13.0".to_string(),
            },
        };

        assert!(client.version_supports_check_runs("1.13.0"));
    }

    #[test]
    fn test_version_supports_check_runs_above_threshold() {
        let client = GiteaClient {
            base_url: "https://test.com".to_string(),
            token: "test".to_string(),
            http_client: Client::new(),
            capabilities: GiteaCapabilities {
                supports_check_runs: false,
                version: "1.21.0".to_string(),
            },
        };

        assert!(client.version_supports_check_runs("1.13.0"));
        assert!(client.version_supports_check_runs("1.14.0"));
        assert!(client.version_supports_check_runs("1.21.3"));
        assert!(client.version_supports_check_runs("2.0.0"));
    }

    #[test]
    fn test_version_supports_check_runs_with_suffix() {
        let client = GiteaClient {
            base_url: "https://test.com".to_string(),
            token: "test".to_string(),
            http_client: Client::new(),
            capabilities: GiteaCapabilities {
                supports_check_runs: false,
                version: "1.13.0+dev".to_string(),
            },
        };

        // Version parsing should handle suffixes like +dev, -rc1, etc.
        assert!(client.version_supports_check_runs("1.13.0+dev"));
        assert!(client.version_supports_check_runs("1.21.0-rc1"));
        assert!(!client.version_supports_check_runs("1.12.0+dev"));
    }

    #[test]
    fn test_version_supports_check_runs_malformed() {
        let client = GiteaClient {
            base_url: "https://test.com".to_string(),
            token: "test".to_string(),
            http_client: Client::new(),
            capabilities: GiteaCapabilities {
                supports_check_runs: false,
                version: "unknown".to_string(),
            },
        };

        // Malformed versions should safely return false
        assert!(!client.version_supports_check_runs("unknown"));
        assert!(!client.version_supports_check_runs(""));
        assert!(!client.version_supports_check_runs("v1.21.0")); // 'v' prefix
    }

    #[test]
    fn test_gitea_client_supports_check_runs() {
        let client_old = GiteaClient {
            base_url: "https://test.com".to_string(),
            token: "test".to_string(),
            http_client: Client::new(),
            capabilities: GiteaCapabilities {
                supports_check_runs: false,
                version: "1.12.0".to_string(),
            },
        };

        let client_new = GiteaClient {
            base_url: "https://test.com".to_string(),
            token: "test".to_string(),
            http_client: Client::new(),
            capabilities: GiteaCapabilities {
                supports_check_runs: true,
                version: "1.21.0".to_string(),
            },
        };

        assert!(!client_old.supports_check_runs());
        assert!(client_new.supports_check_runs());
    }

    #[test]
    fn test_gitea_client_version() {
        let client = GiteaClient {
            base_url: "https://test.com".to_string(),
            token: "test".to_string(),
            http_client: Client::new(),
            capabilities: GiteaCapabilities {
                supports_check_runs: true,
                version: "1.21.3".to_string(),
            },
        };

        assert_eq!(client.version(), "1.21.3");
    }
}
