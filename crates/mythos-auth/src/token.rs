//! HS256 JWT issuance and verification.
//!
//! Token lifetime defaults to 30 days. Revocation is via the `ver` claim:
//! authenticated requests re-check the user's current `token_version`
//! column. Bumping it (logout, password change) invalidates outstanding
//! tokens — clearing only the cookie would leave a stolen bearer token
//! valid until its `exp`.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::AuthError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub iat: i64,
    pub exp: i64,
    pub jti: Uuid,
    pub ver: i64,
}

#[derive(Debug, Clone)]
pub struct TokenConfig {
    pub secret: Arc<[u8]>,
    pub ttl: Duration,
}

impl TokenConfig {
    pub fn new(secret: impl Into<Arc<[u8]>>, ttl: Duration) -> Self {
        Self {
            secret: secret.into(),
            ttl,
        }
    }

    fn encoding_key(&self) -> EncodingKey {
        EncodingKey::from_secret(&self.secret)
    }

    fn decoding_key(&self) -> DecodingKey {
        DecodingKey::from_secret(&self.secret)
    }

    fn validation(&self) -> Validation {
        let mut v = Validation::new(Algorithm::HS256);
        v.leeway = 30;
        v.required_spec_claims = ["exp", "iat", "sub"]
            .into_iter()
            .map(String::from)
            .collect();
        v
    }
}

/// Issue a token for `user_id` carrying the supplied `token_version`.
pub fn issue(cfg: &TokenConfig, user_id: Uuid, token_version: i64) -> Result<String, AuthError> {
    let now = Utc::now().timestamp();
    let exp = now + i64::try_from(cfg.ttl.as_secs()).unwrap_or(i64::MAX);
    let claims = Claims {
        sub: user_id,
        iat: now,
        exp,
        jti: Uuid::now_v7(),
        ver: token_version,
    };
    encode(&Header::new(Algorithm::HS256), &claims, &cfg.encoding_key()).map_err(|err| {
        match err.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::TokenExpired,
            _ => AuthError::TokenInvalid,
        }
    })
}

/// Verify the JWT and return its claims. Does *not* check `ver` against
/// the database — that's the caller's job after fetching the user.
pub fn verify(cfg: &TokenConfig, token: &str) -> Result<Claims, AuthError> {
    let data =
        decode::<Claims>(token, &cfg.decoding_key(), &cfg.validation()).map_err(|err| match err
            .kind()
        {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::TokenExpired,
            _ => AuthError::TokenInvalid,
        })?;
    Ok(data.claims)
}
