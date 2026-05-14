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
            if is_expected_backpressure(self.code) {
                // Controlled "client should retry shortly" responses
                // (transcode session still booting, transcoding
                // disabled at the deployment) come back as 5xx because
                // that's the right HTTP signal for retry-the-request,
                // but they aren't faults — log at debug so they don't
                // poison the operator's view of real failures.
                tracing::debug!(
                    code = self.code,
                    status = %self.status,
                    "controlled backpressure response"
                );
            } else {
                tracing::error!(code = self.code, source = ?self.source, "api server error");
            }
        }
        let body = Json(json!({ "error": self.code }));
        (self.status, body).into_response()
    }
}

/// Codes that surface as 5xx but mean "retry, this is normal" rather
/// than "something is broken." Used to keep the operator log honest:
/// real ERROR lines are real problems.
fn is_expected_backpressure(code: &str) -> bool {
    matches!(code, "session_booting" | "transcoding_disabled")
}

impl From<mythos_db::DbError> for ApiError {
    fn from(err: mythos_db::DbError) -> Self {
        use mythos_db::DbError as E;
        match err {
            E::RootPathTaken => Self::new(StatusCode::CONFLICT, "root_path_taken"),
            E::Decode(msg) => {
                tracing::error!(error = msg, "db decode error");
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal")
            }
            E::Sqlx(e) => Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal").with_source(e),
            E::Migrate(e) => {
                Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal").with_source(e)
            }
        }
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
