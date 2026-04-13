use serde::{Deserialize, Serialize};

/// User stored in the database after OAuth authentication
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AuthenticatedUser {
    #[sqlx(rename = "ROWID")]
    #[allow(dead_code)]
    pub id: i64,
    pub github_id: i64,
    pub github_username: String,
    pub github_avatar_url: Option<String>,
    pub github_access_token: String,
    pub is_admin: bool,
    #[allow(dead_code)]
    pub created_at: chrono::NaiveDateTime,
    #[allow(dead_code)]
    pub last_login: chrono::NaiveDateTime,
}

/// Response sent to frontend after successful login
#[derive(Debug, Serialize, Deserialize)]
pub struct LoginResponse {
    pub token: String,
    pub user: UserInfo,
}

/// User information sent to frontend (without sensitive data)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UserInfo {
    pub github_id: i64,
    pub username: String,
    pub avatar_url: Option<String>,
    pub is_admin: bool,
}

/// GitHub user info from OAuth API
#[derive(Debug, Deserialize)]
pub struct GitHubUserInfo {
    pub id: i64,
    pub login: String,
    pub avatar_url: Option<String>,
}

/// Detailed user information for admin panel
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct UserDetail {
    pub github_id: i64,
    pub github_username: String,
    pub github_avatar_url: Option<String>,
    pub is_admin: bool,
    pub created_at: chrono::NaiveDateTime,
    pub last_login: chrono::NaiveDateTime,
}

/// Attribute path maintainer information
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct AttrPathMaintainer {
    #[sqlx(rename = "ROWID")]
    pub id: i64,
    pub attr_path: String,
    pub github_user_id: i64,
    pub added_at: chrono::NaiveDateTime,
    pub added_by_user_id: Option<i64>,
}

/// Request to add a maintainer to an attr path
#[derive(Debug, Deserialize)]
pub struct AddMaintainerRequest {
    pub github_user_id: i64,
}

/// Request to update user profile
#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    // Future: Add user preferences, notification settings, etc.
}

/// User profile with maintained attr paths
#[derive(Debug, Serialize)]
pub struct UserProfile {
    pub user: UserInfo,
    pub maintained_paths: Vec<String>,
    pub created_at: chrono::NaiveDateTime,
    pub last_login: chrono::NaiveDateTime,
}

/// Attr path maintainer request stored in the database
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AttrPathMaintainerRequest {
    #[sqlx(rename = "ROWID")]
    pub id: i64,
    pub attr_path: String,
    pub github_user_id: i64,
    pub requested_at: chrono::NaiveDateTime,
    pub status: String, // pending, approved, rejected
    pub reviewed_by_user_id: Option<i64>,
    pub reviewed_at: Option<chrono::NaiveDateTime>,
}

/// Request status enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum RequestStatus {
    Pending,
    Approved,
    Rejected,
}

impl RequestStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            RequestStatus::Pending => "pending",
            RequestStatus::Approved => "approved",
            RequestStatus::Rejected => "rejected",
        }
    }
}

/// Request to become a maintainer (used in POST request body if needed)
#[derive(Debug, Deserialize)]
pub struct RequestMaintainerRequest {
    // Empty for now - attr_path comes from URL, user from auth
}

/// Response for maintainer request with additional user info
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct MaintainerRequestDetail {
    pub id: i64,
    pub attr_path: String,
    pub github_user_id: i64,
    pub github_username: String,
    pub github_avatar_url: Option<String>,
    pub requested_at: chrono::NaiveDateTime,
    pub status: String,
    pub reviewed_by_user_id: Option<i64>,
    pub reviewed_by_username: Option<String>,
    pub reviewed_at: Option<chrono::NaiveDateTime>,
}

/// Maintainer info with user details
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct MaintainerDetail {
    pub attr_path: String,
    pub github_user_id: i64,
    pub github_username: String,
    pub github_avatar_url: Option<String>,
    pub added_at: chrono::NaiveDateTime,
    pub added_by_user_id: Option<i64>,
    pub added_by_username: Option<String>,
}

/// GitHub repository permission level
#[derive(Debug, Clone, PartialEq)]
pub enum GitHubPermission {
    None,
    Read,
    Triage,
    Write,
    Maintain,
    Admin,
}

impl GitHubPermission {
    /// Check if this permission level is triage or higher
    pub fn is_triage_or_higher(&self) -> bool {
        matches!(
            self,
            GitHubPermission::Triage
                | GitHubPermission::Write
                | GitHubPermission::Maintain
                | GitHubPermission::Admin
        )
    }
}

/// Response from GitHub API for repository permissions
#[derive(Debug, Deserialize)]
pub struct GitHubRepoPermissionResponse {
    pub permissions: GitHubPermissions,
}

#[derive(Debug, Deserialize)]
pub struct GitHubPermissions {
    pub admin: bool,
    pub maintain: Option<bool>,
    pub push: bool,
    pub triage: Option<bool>,
    pub pull: bool,
}
