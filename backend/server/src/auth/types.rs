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
