//! Axum extractor: resolves [`AuthUser`] from either an
//! `Authorization: Bearer …` header or a `mythos_token` HttpOnly cookie.
//!
//! Resolution order is bearer-then-cookie; a malformed bearer header does
//! not short-circuit the cookie path. After verifying the JWT, the user's
//! current `token_version` is fetched and compared against the `ver`
//! claim — a mismatch is a 401 (covers logout / password-change /
//! admin revocation).

use axum::extract::{FromRef, FromRequestParts};
use axum::http::{HeaderMap, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use cookie::Cookie;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::COOKIE_NAME;
use crate::error::AuthError;
use crate::token::{self, TokenConfig};
use crate::user::UserRepo;

#[derive(Debug, Clone, Copy)]
pub struct AuthUser {
    pub id: Uuid,
    pub is_admin: bool,
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    TokenConfig: FromRef<S>,
    SqlitePool: FromRef<S>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let cfg = TokenConfig::from_ref(state);
        let pool = SqlitePool::from_ref(state);

        let token = extract_token(&parts.headers).ok_or_else(|| reject(AuthError::Unauthorized))?;
        let claims = token::verify(&cfg, &token).map_err(reject)?;

        let repo = UserRepo::new(pool);
        let user = repo
            .find_by_id(claims.sub)
            .await
            .map_err(reject)?
            .ok_or_else(|| reject(AuthError::Unauthorized))?;

        if user.token_version != claims.ver {
            return Err(reject(AuthError::Unauthorized));
        }

        Ok(AuthUser {
            id: user.id,
            is_admin: user.is_admin,
        })
    }
}

fn extract_token(headers: &HeaderMap) -> Option<String> {
    if let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(token) = value.strip_prefix("Bearer ")
    {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let cookie_header = headers.get(header::COOKIE).and_then(|v| v.to_str().ok())?;
    for parsed in Cookie::split_parse(cookie_header) {
        if let Ok(c) = parsed
            && c.name() == COOKIE_NAME
        {
            return Some(c.value().to_string());
        }
    }
    None
}

fn reject(err: AuthError) -> Response {
    match err {
        AuthError::TokenExpired => json_response(
            StatusCode::UNAUTHORIZED,
            r#"{"error":"token_expired"}"#.to_string(),
        ),
        AuthError::Db(e) => {
            tracing::error!(error = ?e, "auth extractor db error");
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                r#"{"error":"internal"}"#.to_string(),
            )
        }
        AuthError::Internal(msg) => {
            tracing::error!(error = msg, "auth extractor internal error");
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                r#"{"error":"internal"}"#.to_string(),
            )
        }
        _ => json_response(
            StatusCode::UNAUTHORIZED,
            r#"{"error":"unauthorized"}"#.to_string(),
        ),
    }
}

fn json_response(status: StatusCode, body: String) -> Response {
    let mut res = (status, body).into_response();
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json"),
    );
    res
}
