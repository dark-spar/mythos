//! Error translation at the HTTP boundary.
//!
//! `mythos-auth::AuthError` is HTTP-agnostic; we map it here so the
//! library stays usable from non-HTTP contexts. The mapping deliberately
//! distinguishes `TokenExpired` from generic `Unauthorized` so the SPA
//! can drop its cached user and redirect to `/login` instead of looping.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str) -> Self {
        Self {
            status,
            code,
            source: None,
        }
    }

    pub fn with_source<E>(mut self, src: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        self.source = Some(Box::new(src));
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!(code = self.code, source = ?self.source, "api server error");
        }
        let body = Json(json!({ "error": self.code }));
        (self.status, body).into_response()
    }
}

impl From<mythos_auth::AuthError> for ApiError {
    fn from(err: mythos_auth::AuthError) -> Self {
        use mythos_auth::AuthError as E;
        match err {
            E::InvalidCredentials => Self::new(StatusCode::UNAUTHORIZED, "invalid_credentials"),
            E::TokenExpired => Self::new(StatusCode::UNAUTHORIZED, "token_expired"),
            E::TokenInvalid | E::Unauthorized => {
                Self::new(StatusCode::UNAUTHORIZED, "unauthorized")
            }
            E::Forbidden => Self::new(StatusCode::FORBIDDEN, "forbidden"),
            E::UsernameTaken => Self::new(StatusCode::CONFLICT, "username_taken"),
            E::Hash(e) => {
                tracing::error!(error = ?e, "argon2 error");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
            E::Db(e) => Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal").with_source(e),
            E::Internal(msg) => {
                tracing::error!(error = msg, "auth internal error");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
