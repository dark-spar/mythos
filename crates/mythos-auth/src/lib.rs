//! Authentication primitives for Mythos.
//!
//! - Argon2id password hashing with pinned parameters ([`password`]).
//! - HS256 JWT issuance and verification ([`token`]).
//! - SQLx-backed user repository ([`user`]).
//! - An [`AuthUser`] axum extractor that accepts either an HttpOnly
//!   `mythos_token` cookie or an `Authorization: Bearer …` header
//!   ([`extractor`]).
//!
//! Domain errors in [`AuthError`] do **not** implement `IntoResponse`;
//! the `mythos-api` crate is responsible for translating them into HTTP
//! responses so this crate stays usable from non-HTTP contexts (CLI,
//! tests, future shims).
//!
//! CSRF posture: the extractor accepts cookie-bound credentials, but
//! mutating endpoints are JSON-only POSTs. Browsers don't send
//! `Content-Type: application/json` cross-origin without preflight, so
//! `SameSite=Lax` cookies are sufficient for Phase 1a. Revisit when
//! adding form posts.

pub mod error;
pub mod extractor;
pub mod password;
pub mod token;
pub mod user;

pub use error::AuthError;
pub use extractor::{AdminUser, AuthUser};
pub use token::{Claims, TokenConfig};
pub use user::{NewUser, User, UserRepo};

/// Name of the HttpOnly cookie that carries the JWT for browser clients.
pub const COOKIE_NAME: &str = "mythos_token";
