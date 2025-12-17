use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};

use super::types::AuthenticatedUser;

/// JWT Claims structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,      // Subject (github_id as string)
    pub github_id: i64,   // GitHub user ID
    pub username: String, // GitHub username
    pub is_admin: bool,   // Admin status
    pub exp: u64,         // Expiration time (Unix timestamp)
    pub iat: u64,         // Issued at (Unix timestamp)
}

/// JWT service for creating and validating tokens
#[derive(Clone)]
pub struct JwtService {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    validation: Validation,
}

impl JwtService {
    /// Create a new JWT service with the provided secret
    pub fn new(secret: &str) -> Self {
        let encoding_key = EncodingKey::from_secret(secret.as_bytes());
        let decoding_key = DecodingKey::from_secret(secret.as_bytes());

        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_exp = true;

        Self {
            encoding_key,
            decoding_key,
            validation,
        }
    }

    /// Create a JWT token for a user (7-day expiration)
    pub fn create_token(
        &self,
        user: &AuthenticatedUser,
    ) -> Result<String, jsonwebtoken::errors::Error> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        let exp = now + (7 * 24 * 60 * 60); // 7 days from now

        let claims = Claims {
            sub: user.github_id.to_string(),
            github_id: user.github_id,
            username: user.github_username.clone(),
            is_admin: user.is_admin,
            exp,
            iat: now,
        };

        encode(&Header::default(), &claims, &self.encoding_key)
    }

    /// Validate a JWT token and extract claims
    pub fn validate_token(&self, token: &str) -> Result<Claims, jsonwebtoken::errors::Error> {
        let token_data = decode::<Claims>(token, &self.decoding_key, &self.validation)?;
        Ok(token_data.claims)
    }
}
