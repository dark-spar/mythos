//! Argon2id password hashing with pinned parameters.
//!
//! `Argon2::default()` has shifted between minor versions; we pin our own
//! [`params`] so a dependency bump can't silently change the cost.

use std::sync::OnceLock;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use rand_core::OsRng;

use crate::error::AuthError;

/// 19 MiB memory cost, 2 iterations, parallelism 1. Roughly the OWASP 2023
/// recommendation for argon2id. Output length is the default (32 bytes).
fn params() -> Params {
    Params::new(19 * 1024, 2, 1, None).expect("hardcoded argon2 params are valid")
}

fn hasher() -> Argon2<'static> {
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params())
}

/// Hash `password` and return a PHC-format string suitable for storage.
pub fn hash(password: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    let phc = hasher().hash_password(password.as_bytes(), &salt)?;
    Ok(phc.to_string())
}

/// Verify `password` against a stored PHC hash.
pub fn verify(password: &str, phc: &str) -> Result<(), AuthError> {
    let parsed = PasswordHash::new(phc)?;
    hasher()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| AuthError::InvalidCredentials)
}

/// Run a verify against a constant-time dummy hash. Called when a username
/// lookup misses so login latency does not leak user existence.
pub fn verify_dummy(password: &str) {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let phc =
        DUMMY.get_or_init(|| hash("dummy-password").expect("dummy hash generation must succeed"));
    let _ = verify(password, phc);
}
