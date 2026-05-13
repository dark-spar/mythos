//! `/api/auth/*` and `/api/users/me` handlers.

use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use cookie::{Cookie, SameSite};
use mythos_auth::{
    AuthError, AuthUser, COOKIE_NAME, NewUser, TokenConfig, User, UserRepo, password, token,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::CookieConfig;
use crate::error::{ApiError, ApiResult};

#[derive(Serialize)]
pub struct StatusResponse {
    pub bootstrapped: bool,
}

pub async fn status(State(pool): State<SqlitePool>) -> ApiResult<Json<StatusResponse>> {
    let count = UserRepo::new(pool).count().await?;
    Ok(Json(StatusResponse {
        bootstrapped: count > 0,
    }))
}

#[derive(Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub user: User,
}

pub async fn register(
    State(pool): State<SqlitePool>,
    State(cfg): State<TokenConfig>,
    State(cookies): State<CookieConfig>,
    Json(creds): Json<Credentials>,
) -> ApiResult<Response> {
    if creds.username.trim().is_empty() || creds.password.len() < 8 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_credentials",
        ));
    }
    let hashed = password::hash(&creds.password)?;
    let user = UserRepo::new(pool)
        .insert_first(NewUser {
            username: creds.username.trim().to_string(),
            password_hash: hashed,
            is_admin: true,
        })
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::FORBIDDEN, "forbidden"))?;

    let issued = token::issue(&cfg, user.id, user.token_version)?;
    Ok(auth_response(&cookies, cfg.ttl, issued, user))
}

pub async fn login(
    State(pool): State<SqlitePool>,
    State(cfg): State<TokenConfig>,
    State(cookies): State<CookieConfig>,
    Json(creds): Json<Credentials>,
) -> ApiResult<Response> {
    let repo = UserRepo::new(pool);
    let user = match repo.find_by_username(&creds.username).await? {
        Some(u) => {
            password::verify(&creds.password, &u.password_hash)?;
            u
        }
        None => {
            // Constant-time decoy so login latency doesn't leak whether
            // the username exists.
            password::verify_dummy(&creds.password);
            return Err(AuthError::InvalidCredentials.into());
        }
    };

    let issued = token::issue(&cfg, user.id, user.token_version)?;
    Ok(auth_response(&cookies, cfg.ttl, issued, user))
}

pub async fn logout(
    State(pool): State<SqlitePool>,
    State(cookies): State<CookieConfig>,
    auth: AuthUser,
) -> ApiResult<Response> {
    UserRepo::new(pool).bump_token_version(auth.id).await?;
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, cookie_header(&clear_cookie(&cookies)));
    Ok(res)
}

pub async fn me(State(pool): State<SqlitePool>, auth: AuthUser) -> ApiResult<Json<User>> {
    let user = UserRepo::new(pool)
        .find_by_id(auth.id)
        .await?
        .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    Ok(Json(user))
}

fn auth_response(cookies: &CookieConfig, ttl: Duration, token: String, user: User) -> Response {
    let body = Json(AuthResponse {
        token: token.clone(),
        user,
    });
    let mut res = (StatusCode::OK, body).into_response();
    let cookie = build_cookie(cookies, &token, ttl);
    res.headers_mut()
        .insert(header::SET_COOKIE, cookie_header(&cookie));
    res
}

fn build_cookie<'a>(cookies: &CookieConfig, value: &'a str, ttl: Duration) -> Cookie<'a> {
    Cookie::build((COOKIE_NAME, value))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .secure(cookies.secure)
        .max_age(cookie::time::Duration::seconds(
            i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX),
        ))
        .build()
}

fn clear_cookie(cookies: &CookieConfig) -> Cookie<'static> {
    Cookie::build((COOKIE_NAME, ""))
        .http_only(true)
        .same_site(SameSite::Lax)
        .path("/")
        .secure(cookies.secure)
        .max_age(cookie::time::Duration::ZERO)
        .build()
}

fn cookie_header(c: &Cookie<'_>) -> HeaderValue {
    HeaderValue::from_str(&c.to_string()).expect("cookie serializes to header value")
}
