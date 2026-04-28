use std::sync::Arc;

use anyhow::{Context, Result, bail};
use reqwest::{Client, header};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// GitLab API client
///
/// GitLab uses a different API from GitHub:
/// - Commit Status API (no Check Runs)
/// - Merge Request Notes for rich output (vs check run annotations)
/// - Project-based authentication with access tokens
pub struct GitLabClient {
    base_url: String,
    token: String,
    http_client: Client,
}

impl GitLabClient {
    /// Create a new GitLab client
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

        Ok(Self {
            base_url,
            token,
            http_client,
        })
    }

    /// Build authorization header
    fn auth_header(&self) -> header::HeaderValue {
        header::HeaderValue::from_str(&format!("Bearer {}", self.token))
            .expect("Token should be valid header value")
    }
}

// ==================================================================
// Commit Status API
// ==================================================================

#[derive(Debug, Serialize)]
pub struct CreateCommitStatusRequest {
    pub state: CommitStatusState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitStatusState {
    Pending,
    Running,
    Success,
    Failed,
    Canceled,
}

#[derive(Debug, Deserialize)]
pub struct CommitStatus {
    pub id: i64,
    pub status: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

impl GitLabClient {
    /// Create or update a commit status
    ///
    /// GitLab API: POST /projects/:id/statuses/:sha
    pub async fn create_commit_status(
        &self,
        project_id: i64,
        sha: &str,
        request: CreateCommitStatusRequest,
    ) -> Result<CommitStatus> {
        let url = format!(
            "{}/api/v4/projects/{}/statuses/{}",
            self.base_url, project_id, sha
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
}

// ==================================================================
// Merge Request Notes (Comments)
// ==================================================================

#[derive(Debug, Serialize)]
pub struct CreateMergeRequestNoteRequest {
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct MergeRequestNote {
    pub id: i64,
    pub body: String,
    pub author: User,
    #[serde(default)]
    pub system: bool,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub id: i64,
    pub username: String,
}

impl GitLabClient {
    /// Create a merge request note (comment)
    ///
    /// GitLab API: POST /projects/:id/merge_requests/:merge_request_iid/notes
    pub async fn create_merge_request_note(
        &self,
        project_id: i64,
        mr_iid: i64,
        body: &str,
    ) -> Result<MergeRequestNote> {
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/notes",
            self.base_url, project_id, mr_iid
        );

        let request = CreateMergeRequestNoteRequest {
            body: body.to_string(),
        };

        let response = self
            .http_client
            .post(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .json(&request)
            .send()
            .await
            .context("Failed to create MR note")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to create MR note ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse MR note response")
    }

    /// Update a merge request note
    ///
    /// GitLab API: PUT /projects/:id/merge_requests/:merge_request_iid/notes/:note_id
    pub async fn update_merge_request_note(
        &self,
        project_id: i64,
        mr_iid: i64,
        note_id: i64,
        body: &str,
    ) -> Result<MergeRequestNote> {
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/notes/{}",
            self.base_url, project_id, mr_iid, note_id
        );

        let request = CreateMergeRequestNoteRequest {
            body: body.to_string(),
        };

        let response = self
            .http_client
            .put(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .json(&request)
            .send()
            .await
            .context("Failed to update MR note")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to update MR note ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse MR note response")
    }

    /// List merge request notes
    ///
    /// GitLab API: GET /projects/:id/merge_requests/:merge_request_iid/notes
    pub async fn list_merge_request_notes(
        &self,
        project_id: i64,
        mr_iid: i64,
    ) -> Result<Vec<MergeRequestNote>> {
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/notes",
            self.base_url, project_id, mr_iid
        );

        let response = self
            .http_client
            .get(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("Failed to list MR notes")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to list MR notes ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse MR notes response")
    }

    /// Find a "sticky" comment by marker
    ///
    /// Sticky comments use a marker at the end like:
    /// <!-- eka-ci-marker: change-summary -->
    pub async fn find_sticky_comment(
        &self,
        project_id: i64,
        mr_iid: i64,
        marker: &str,
    ) -> Result<Option<i64>> {
        let notes = self.list_merge_request_notes(project_id, mr_iid).await?;

        let marker_text = format!("<!-- eka-ci-marker: {} -->", marker);

        for note in notes {
            if note.body.contains(&marker_text) {
                return Ok(Some(note.id));
            }
        }

        Ok(None)
    }

    /// Post or update a sticky comment
    ///
    /// If a comment with the marker exists, update it. Otherwise, create a new one.
    pub async fn post_or_update_sticky_comment(
        &self,
        project_id: i64,
        mr_iid: i64,
        marker: &str,
        body: &str,
    ) -> Result<MergeRequestNote> {
        let body_with_marker = format!("{}\n\n<!-- eka-ci-marker: {} -->", body, marker);

        if let Some(note_id) = self.find_sticky_comment(project_id, mr_iid, marker).await? {
            debug!(
                "Updating existing sticky comment {} on MR {}",
                note_id, mr_iid
            );
            self.update_merge_request_note(project_id, mr_iid, note_id, &body_with_marker)
                .await
        } else {
            debug!("Creating new sticky comment on MR {}", mr_iid);
            self.create_merge_request_note(project_id, mr_iid, &body_with_marker)
                .await
        }
    }
}

// ==================================================================
// Merge Request Operations
// ==================================================================

#[derive(Debug, Serialize)]
pub struct MergeMergeRequestRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_commit_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub squash_commit_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub should_remove_source_branch: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_when_pipeline_succeeds: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MergeRequest {
    pub id: i64,
    pub iid: i64,
    pub state: String,
    pub sha: String,
    pub merge_status: String,
    #[serde(default)]
    pub merged_at: Option<String>,
    pub source_branch: String,
    pub target_branch: String,
    pub project_id: i64,
}

#[derive(Debug, Deserialize)]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub path_with_namespace: String,
    pub default_branch: String,
    #[serde(default)]
    pub merge_method: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProjectMember {
    pub id: i64,
    pub username: String,
    pub access_level: i32,
}

impl GitLabClient {
    /// Merge a merge request
    ///
    /// GitLab API: PUT /projects/:id/merge_requests/:merge_request_iid/merge
    pub async fn merge_merge_request(
        &self,
        project_id: i64,
        mr_iid: i64,
        request: MergeMergeRequestRequest,
    ) -> Result<()> {
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/merge",
            self.base_url, project_id, mr_iid
        );

        let response = self
            .http_client
            .put(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .json(&request)
            .send()
            .await
            .context("Failed to merge MR")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to merge MR ({}): {}", status, body);
        }

        Ok(())
    }

    /// Get merge request details
    ///
    /// GitLab API: GET /projects/:id/merge_requests/:merge_request_iid
    pub async fn get_merge_request(&self, project_id: i64, mr_iid: i64) -> Result<MergeRequest> {
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}",
            self.base_url, project_id, mr_iid
        );

        let response = self
            .http_client
            .get(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("Failed to get MR")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to get MR ({}): {}", status, body);
        }

        response.json().await.context("Failed to parse MR response")
    }

    /// Get project details
    ///
    /// GitLab API: GET /projects/:id
    pub async fn get_project(&self, project_id: i64) -> Result<Project> {
        let url = format!("{}/api/v4/projects/{}", self.base_url, project_id);

        let response = self
            .http_client
            .get(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("Failed to get project")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to get project ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse project response")
    }

    /// Get project member (to check permissions)
    ///
    /// GitLab API: GET /projects/:id/members/:user_id
    /// or GET /projects/:id/members/all/:user_id (includes inherited)
    pub async fn get_project_member(&self, project_id: i64, user_id: i64) -> Result<ProjectMember> {
        // Use "all" to include inherited permissions from groups
        let url = format!(
            "{}/api/v4/projects/{}/members/all/{}",
            self.base_url, project_id, user_id
        );

        let response = self
            .http_client
            .get(&url)
            .header(header::AUTHORIZATION, self.auth_header())
            .send()
            .await
            .context("Failed to get project member")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Failed to get project member ({}): {}", status, body);
        }

        response
            .json()
            .await
            .context("Failed to parse project member response")
    }

    /// Check if user has at least Developer access (access_level >= 30)
    ///
    /// GitLab access levels:
    /// - 10: Guest
    /// - 20: Reporter
    /// - 30: Developer
    /// - 40: Maintainer
    /// - 50: Owner
    pub async fn has_developer_access(&self, project_id: i64, user_id: i64) -> Result<bool> {
        match self.get_project_member(project_id, user_id).await {
            Ok(member) => Ok(member.access_level >= 30),
            Err(_) => Ok(false), // If we can't fetch, assume no access
        }
    }
}

// ==================================================================
// Reactions (Awards API)
// ==================================================================

#[derive(Debug, Serialize)]
pub struct CreateAwardRequest {
    pub name: String,
}

impl GitLabClient {
    /// Add an emoji reaction to a merge request note
    ///
    /// GitLab API: POST /projects/:id/merge_requests/:merge_request_iid/notes/:note_id/award_emoji
    pub async fn add_note_reaction(
        &self,
        project_id: i64,
        mr_iid: i64,
        note_id: i64,
        emoji: &str,
    ) -> Result<()> {
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/notes/{}/award_emoji",
            self.base_url, project_id, mr_iid, note_id
        );

        let request = CreateAwardRequest {
            name: emoji.to_string(),
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
}
