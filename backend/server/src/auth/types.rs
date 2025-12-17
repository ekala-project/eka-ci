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
