use std::future::Future;

use axum::Json;
use axum::extract::{FromRef, FromRequestParts};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::jwt::{Claims, JwtService};

/// Authenticated user extracted from request
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub claims: Claims,
}

impl AuthUser {
    pub fn github_id(&self) -> i64 {
        self.claims.github_id
    }

    pub fn is_admin(&self) -> bool {
        self.claims.is_admin
    }
}

/// Admin user (requires is_admin = true)
#[derive(Debug, Clone)]
pub struct AdminUser {
    #[allow(dead_code)]
    pub user: AuthUser,
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

            Ok(AuthUser { claims })
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

            Ok(AdminUser { user })
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
