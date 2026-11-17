use std::collections::HashSet;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
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
use super::oauth_state;
use super::types::{AuthenticatedUser, GitHubUserInfo, UserInfo};
use crate::secret::Redacted;

/// OAuth configuration
#[derive(Clone)]
pub struct OAuthConfig {
    pub client_id: String,
    // M2: wrap so `Debug` (if ever derived or manually invoked on a
    // struct embedding this) cannot leak the client secret.
    pub client_secret: Redacted<String>,
    pub redirect_url: String,
    /// GitHub ids declared as admins in `ekaci.toml`; granted admin on login.
    pub admin_github_ids: Arc<HashSet<i64>>,
}

impl OAuthConfig {
    /// Whether auth cookies should carry `Secure` (redirect URL is HTTPS).
    fn secure(&self) -> bool {
        self.redirect_url.starts_with("https://")
    }

    /// Create OAuth client
    fn create_client(&self) -> Result<BasicClient, String> {
        let client = BasicClient::new(
            ClientId::new(self.client_id.clone()),
            Some(ClientSecret::new(self.client_secret.expose().clone())),
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

/// Query parameters from GitHub OAuth callback.
///
/// GitHub sends `code` + `state` on success, or `error` (+ description)
/// when the user denies access.
#[derive(Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// Handle /github/auth/login - redirect to GitHub.
///
/// The `state` sent to GitHub is also stored in a short-lived cookie so the
/// callback can verify that it completes a login this browser started.
pub async fn handle_login(State(state): State<AppState>) -> Response {
    let client = match state.oauth_config.create_client() {
        Ok(client) => client,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e })),
            )
                .into_response();
        },
    };

    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("read:user".to_string()))
        .url();

    let cookie = oauth_state::set_state_cookie(csrf_token.secret(), state.oauth_config.secure());
    let mut response = Redirect::to(auth_url.as_str()).into_response();
    match HeaderValue::from_str(&cookie) {
        Ok(value) => {
            response.headers_mut().insert(SET_COOKIE, value);
        },
        Err(e) => {
            tracing::error!("OAuth state cookie is not a valid header: {}", e);
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        },
    }
    response
}

/// Handle /github/auth/callback - verify state, exchange code, sign in.
///
/// Always redirects the browser to the frontend's `/auth/complete` page,
/// with either `#token=<jwt>` or `#error=<message>`.
pub async fn handle_callback(
    headers: HeaderMap,
    Query(params): Query<CallbackParams>,
    State(state): State<AppState>,
) -> Response {
    let secure = state.oauth_config.secure();
    let fragment = match complete_login(&headers, params, &state).await {
        Ok(token) => oauth_state::token_fragment(&token),
        Err(message) => {
            tracing::warn!("GitHub login failed: {}", message);
            oauth_state::error_fragment(&message)
        },
    };
    oauth_state::complete_redirect(&fragment, secure)
}

/// Validate the callback and return a JWT for the signed-in user.
async fn complete_login(
    headers: &HeaderMap,
    params: CallbackParams,
    state: &AppState,
) -> Result<String, String> {
    if let Some(error) = params.error {
        return Err(params.error_description.unwrap_or(error));
    }
    let expected = oauth_state::read_state_cookie(headers);
    if !oauth_state::state_matches(expected.as_deref(), params.state.as_deref()) {
        return Err("Login session expired or did not match. Please try again.".to_string());
    }
    let code = params
        .code
        .ok_or_else(|| "GitHub did not return an authorization code.".to_string())?;

    let access_token = exchange_code(&state.oauth_config, code).await?;
    let github_user = fetch_github_user(&access_token)
        .await
        .map_err(|e| format!("Failed to fetch user info: {}", e))?;

    let config_admin = state
        .oauth_config
        .admin_github_ids
        .contains(&github_user.id);
    let user = upsert_user(&state.db, &github_user, &access_token, config_admin)
        .await
        .map_err(|e| format!("Failed to save user: {}", e))?;

    state
        .jwt_service
        .create_token(&user)
        .map_err(|e| format!("Failed to create token: {}", e))
}

/// Exchange an authorization code for a GitHub access token.
async fn exchange_code(config: &OAuthConfig, code: String) -> Result<String, String> {
    let client = config.create_client()?;
    let token_result = client
        .exchange_code(AuthorizationCode::new(code))
        .request_async(oauth2::reqwest::async_http_client)
        .await
        .map_err(|e| format!("Failed to exchange code: {}", e))?;
    Ok(token_result.access_token().secret().clone())
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
    .bind(user.github_id)
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
    config_admin: bool,
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
                 is_admin = (is_admin OR ?),
                 last_login = CURRENT_TIMESTAMP
             WHERE github_id = ?",
        )
        .bind(&github_user.login)
        .bind(&github_user.avatar_url)
        .bind(access_token)
        .bind(config_admin)
        .bind(github_user.id)
        .execute(db)
        .await?;

        // Update fields for return
        user.github_username = github_user.login.clone();
        user.github_avatar_url = github_user.avatar_url.clone();
        user.github_access_token = access_token.to_string();
        user.is_admin |= config_admin;

        Ok(user)
    } else {
        // Insert new user (admin only if declared in ekaci.toml)
        sqlx::query(
            "INSERT INTO AuthenticatedUsers
             (github_id, github_username, github_avatar_url, github_access_token, is_admin)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(github_user.id)
        .bind(&github_user.login)
        .bind(&github_user.avatar_url)
        .bind(access_token)
        .bind(config_admin)
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
