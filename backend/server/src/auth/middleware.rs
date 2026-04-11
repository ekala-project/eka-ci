use std::future::Future;

use axum::Json;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use sqlx::SqlitePool;

use super::jwt::{Claims, JwtService};

/// Authenticated user extracted from request
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub claims: Claims,
    pub github_id: i64,
}

impl AuthUser {
    pub fn is_admin(&self) -> bool {
        self.claims.is_admin
    }

    /// Check if user is a maintainer of a specific attr path
    pub async fn is_maintainer_of(
        &self,
        pool: &SqlitePool,
        attr_path: &str,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query_scalar::<_, bool>(
            r#"
            SELECT COUNT(*) > 0
            FROM AttrPathMaintainers
            WHERE attr_path = ? AND github_user_id = ?
            "#,
        )
        .bind(attr_path)
        .bind(self.github_id)
        .fetch_one(pool)
        .await?;

        Ok(result)
    }

    /// Check if user can modify a build (admin or maintainer)
    pub async fn can_modify_build(
        &self,
        pool: &SqlitePool,
        attr_path: &str,
    ) -> Result<bool, sqlx::Error> {
        if self.is_admin() {
            return Ok(true);
        }
        self.is_maintainer_of(pool, attr_path).await
    }
}

/// Admin user (requires is_admin = true)
#[derive(Debug, Clone)]
pub struct AdminUser {
    #[allow(dead_code)]
    pub user: AuthUser,
    pub github_id: i64,
}

/// Extract AuthUser from request (validates JWT)
impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    JwtService: FromRef<S>,
{
    type Rejection = AuthError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let auth_header = parts
            .headers
            .get("Authorization")
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string());

        let jwt_service = JwtService::from_ref(state);

        async move {
            // Extract Authorization header
            let auth_header = auth_header.ok_or(AuthError::MissingToken)?;

            // Check if it starts with "Bearer "
            let token = auth_header
                .strip_prefix("Bearer ")
                .ok_or(AuthError::InvalidToken)?;

            // Validate token
            let claims = jwt_service
                .validate_token(token)
                .map_err(|_| AuthError::InvalidToken)?;

            let github_id = claims.github_id;

            Ok(AuthUser { claims, github_id })
        }
    }
}

/// Extract AdminUser from request (validates JWT + admin status)
impl<S> FromRequestParts<S> for AdminUser
where
    S: Send + Sync,
    JwtService: FromRef<S>,
{
    type Rejection = AuthError;

    fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> impl Future<Output = Result<Self, Self::Rejection>> + Send {
        let fut = AuthUser::from_request_parts(parts, state);

        async move {
            let user = fut.await?;

            if !user.is_admin() {
                return Err(AuthError::Forbidden);
            }

            let github_id = user.github_id;

            Ok(AdminUser { user, github_id })
        }
    }
}

/// Authentication errors
#[derive(Debug)]
pub enum AuthError {
    MissingToken,
    InvalidToken,
    Forbidden,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AuthError::MissingToken => (StatusCode::UNAUTHORIZED, "Missing authorization token"),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid or expired token"),
            AuthError::Forbidden => (StatusCode::FORBIDDEN, "Insufficient permissions"),
        };

        let body = Json(json!({
            "error": message,
        }));

        (status, body).into_response()
    }
}
