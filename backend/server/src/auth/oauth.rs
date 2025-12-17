use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, RedirectUrl, Scope,
    TokenResponse, TokenUrl,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;

use super::jwt::JwtService;
use super::middleware::AuthUser;
use super::types::{AuthenticatedUser, GitHubUserInfo, LoginResponse, UserInfo};

/// OAuth configuration
#[derive(Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
}

impl OAuthConfig {
    /// Create OAuth client
    fn create_client(&self) -> Result<BasicClient, String> {
        let client = BasicClient::new(
            ClientId::new(self.client_id.clone()),
            Some(ClientSecret::new(self.client_secret.clone())),
            AuthUrl::new("https://github.com/login/oauth/authorize".to_string())
                .map_err(|e| format!("Invalid auth URL: {}", e))?,
            Some(
                TokenUrl::new("https://github.com/login/oauth/access_token".to_string())
                    .map_err(|e| format!("Invalid token URL: {}", e))?,
            ),
        )
        .set_redirect_uri(
            RedirectUrl::new(self.redirect_url.clone())
                .map_err(|e| format!("Invalid redirect URL: {}", e))?,
        );

        Ok(client)
    }
}

/// App state for OAuth handlers
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub jwt_service: JwtService,
    pub oauth_config: OAuthConfig,
}

/// Query parameters from GitHub OAuth callback
#[derive(Deserialize)]
pub struct CallbackParams {
    pub code: String,
    #[allow(dead_code)]
    pub state: Option<String>,
}

/// Handle /auth/github/login - redirect to GitHub
pub async fn handle_login(State(state): State<AppState>) -> Result<Redirect, Response> {
    let client = state.oauth_config.create_client().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response()
    })?;

    let (auth_url, _csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("read:user".to_string()))
        .url();

    Ok(Redirect::temporary(auth_url.as_str()))
}

/// Handle /auth/github/callback - exchange code for token
pub async fn handle_callback(
    Query(params): Query<CallbackParams>,
    State(state): State<AppState>,
) -> Result<Json<LoginResponse>, Response> {
    // Create OAuth client
    let client = state.oauth_config.create_client().map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e })),
        )
            .into_response()
    })?;

    // Exchange authorization code for access token
    let token_result = client
        .exchange_code(AuthorizationCode::new(params.code))
        .request_async(oauth2::reqwest::async_http_client)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("Failed to exchange code: {}", e) })),
            )
                .into_response()
        })?;

    let access_token = token_result.access_token().secret();

    // Fetch user info from GitHub
    let github_user = fetch_github_user(access_token).await.map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to fetch user info: {}", e) })),
        )
            .into_response()
    })?;

    // Upsert user in database
    let user = upsert_user(&state.db, &github_user, access_token)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("Failed to save user: {}", e) })),
            )
                .into_response()
        })?;

    // Create JWT
    let token = state.jwt_service.create_token(&user).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Failed to create token: {}", e) })),
        )
            .into_response()
    })?;

    Ok(Json(LoginResponse {
        token,
        user: UserInfo {
            github_id: user.github_id,
            username: user.github_username,
            avatar_url: user.github_avatar_url,
            is_admin: user.is_admin,
        },
    }))
}

/// Handle /auth/me - get current user info
pub async fn handle_me(
    user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<UserInfo>, Response> {
    // Fetch user from database to get latest info including avatar
    let db_user = sqlx::query_as::<_, AuthenticatedUser>(
        "SELECT ROWID, * FROM AuthenticatedUsers WHERE github_id = ?",
    )
    .bind(user.github_id())
    .fetch_optional(&state.db)
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("Database error: {}", e) })),
        )
            .into_response()
    })?
    .ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "User not found" })),
        )
            .into_response()
    })?;

    Ok(Json(UserInfo {
        github_id: db_user.github_id,
        username: db_user.github_username,
        avatar_url: db_user.github_avatar_url,
        is_admin: db_user.is_admin,
    }))
}

/// Fetch user info from GitHub API
async fn fetch_github_user(access_token: &str) -> Result<GitHubUserInfo, String> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("User-Agent", "eka-ci")
        .send()
        .await
        .map_err(|e| format!("Failed to fetch user: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("GitHub API error: {}", response.status()));
    }

    response
        .json::<GitHubUserInfo>()
        .await
        .map_err(|e| format!("Failed to parse user info: {}", e))
}

/// Insert or update user in database
async fn upsert_user(
    db: &SqlitePool,
    github_user: &GitHubUserInfo,
    access_token: &str,
) -> Result<AuthenticatedUser, sqlx::Error> {
    // Check if user exists
    let existing_user = sqlx::query_as::<_, AuthenticatedUser>(
        "SELECT ROWID, * FROM AuthenticatedUsers WHERE github_id = ?",
    )
    .bind(github_user.id)
    .fetch_optional(db)
    .await?;

    if let Some(mut user) = existing_user {
        // Update existing user
        sqlx::query(
            "UPDATE AuthenticatedUsers
             SET github_username = ?,
                 github_avatar_url = ?,
                 github_access_token = ?,
                 last_login = CURRENT_TIMESTAMP
             WHERE github_id = ?",
        )
        .bind(&github_user.login)
        .bind(&github_user.avatar_url)
        .bind(access_token)
        .bind(github_user.id)
        .execute(db)
        .await?;

        // Update fields for return
        user.github_username = github_user.login.clone();
        user.github_avatar_url = github_user.avatar_url.clone();
        user.github_access_token = access_token.to_string();

        Ok(user)
    } else {
        // Insert new user (not admin by default)
        sqlx::query(
            "INSERT INTO AuthenticatedUsers
             (github_id, github_username, github_avatar_url, github_access_token, is_admin)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(github_user.id)
        .bind(&github_user.login)
        .bind(&github_user.avatar_url)
        .bind(access_token)
        .bind(false)
        .execute(db)
        .await?;

        // Fetch the newly inserted user
        sqlx::query_as::<_, AuthenticatedUser>(
            "SELECT ROWID, * FROM AuthenticatedUsers WHERE github_id = ?",
        )
        .bind(github_user.id)
        .fetch_one(db)
        .await
    }
}
