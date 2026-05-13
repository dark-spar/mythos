use std::sync::Arc;
use std::time::Duration;

use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use mythos_auth::{AuthError, Claims, TokenConfig, token};
use serde::Serialize;
use uuid::Uuid;

fn cfg(ttl_secs: u64) -> TokenConfig {
    TokenConfig::new(
        Arc::<[u8]>::from(&b"super-secret-key-32-bytes-or-more-here"[..]),
        Duration::from_secs(ttl_secs),
    )
}

#[test]
fn issue_then_verify_roundtrip() {
    let c = cfg(60);
    let user = Uuid::now_v7();
    let issued = token::issue(&c, user, 0).expect("issue");
    let claims = token::verify(&c, &issued).expect("verify");
    assert_eq!(claims.sub, user);
    assert_eq!(claims.ver, 0);
    assert!(claims.exp > claims.iat);
}

#[test]
fn wrong_secret_rejects_token() {
    let issued = token::issue(&cfg(60), Uuid::now_v7(), 0).expect("issue");
    let other = TokenConfig::new(
        Arc::<[u8]>::from(&b"different-secret-different-bytes-32-bytes!"[..]),
        Duration::from_secs(60),
    );
    let err = token::verify(&other, &issued).unwrap_err();
    assert!(
        matches!(err, AuthError::TokenInvalid),
        "expected TokenInvalid, got {err:?}"
    );
}

#[test]
fn expired_token_is_rejected_with_token_expired() {
    let secret: Arc<[u8]> = Arc::from(&b"secret-of-sufficient-length-for-jwt-hs256"[..]);
    let c = TokenConfig::new(secret.clone(), Duration::from_secs(60));

    // Hand-roll an expired-in-the-past token. Leeway is 30s; we use a
    // wide margin to avoid any flakiness.
    let now = chrono::Utc::now().timestamp();
    let claims = Claims {
        sub: Uuid::now_v7(),
        iat: now - 7200,
        exp: now - 3600,
        jti: Uuid::now_v7(),
        ver: 0,
    };
    let issued = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(&secret),
    )
    .expect("hand-rolled encode");

    let err = token::verify(&c, &issued).unwrap_err();
    assert!(
        matches!(err, AuthError::TokenExpired),
        "expected TokenExpired, got {err:?}"
    );
}

#[test]
fn alg_none_token_is_rejected() {
    // Hand-construct an unsigned JWT. We pinned HS256 in Validation, so
    // jsonwebtoken must reject this no matter what the body looks like.
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    let header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        r#"{"sub":"00000000-0000-7000-8000-000000000000","iat":0,"exp":99999999999,"jti":"00000000-0000-7000-8000-000000000000","ver":0}"#,
    );
    let unsigned = format!("{header}.{payload}.");

    let err = token::verify(&cfg(60), &unsigned).unwrap_err();
    assert!(
        matches!(err, AuthError::TokenInvalid),
        "expected TokenInvalid for alg=none, got {err:?}"
    );
}

#[test]
fn token_missing_required_claim_is_rejected() {
    // Encode with our secret + HS256 but a payload that omits `exp`.
    // required_spec_claims = {exp, iat, sub} so this must fail verify.
    #[derive(Serialize)]
    struct PartialClaims {
        sub: Uuid,
        iat: i64,
    }
    let secret: Arc<[u8]> = Arc::from(&b"secret-of-sufficient-length-for-jwt-hs256"[..]);
    let c = TokenConfig::new(secret.clone(), Duration::from_secs(60));
    let issued = encode(
        &Header::new(Algorithm::HS256),
        &PartialClaims {
            sub: Uuid::now_v7(),
            iat: 0,
        },
        &EncodingKey::from_secret(&secret),
    )
    .expect("encode partial claims");

    let err = token::verify(&c, &issued).unwrap_err();
    assert!(
        matches!(err, AuthError::TokenInvalid),
        "expected TokenInvalid for missing exp, got {err:?}"
    );
}
