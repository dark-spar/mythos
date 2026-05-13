use mythos_auth::password;

#[test]
fn hash_then_verify_roundtrip() {
    let phc = password::hash("correct horse battery staple").expect("hash");
    password::verify("correct horse battery staple", &phc).expect("verify");
}

#[test]
fn wrong_password_is_rejected() {
    let phc = password::hash("correct horse battery staple").expect("hash");
    let err = password::verify("Tr0ub4dor&3", &phc).unwrap_err();
    assert!(
        matches!(err, mythos_auth::AuthError::InvalidCredentials),
        "expected InvalidCredentials, got {err:?}"
    );
}

#[test]
fn hashes_use_unique_salt() {
    let a = password::hash("same").expect("hash a");
    let b = password::hash("same").expect("hash b");
    assert_ne!(a, b, "two hashes of the same password must differ (salt)");
}

#[test]
fn malformed_phc_string_errors() {
    let err = password::verify("anything", "not-a-phc-string").unwrap_err();
    assert!(
        matches!(err, mythos_auth::AuthError::Hash(_)),
        "expected Hash error, got {err:?}"
    );
}

#[test]
fn dummy_verify_runs_without_panic() {
    // We don't assert timing parity — that's flaky and OS-dependent. We
    // just exercise the dummy verify path so it isn't dead code and so
    // the OnceLock initialization is covered.
    password::verify_dummy("whatever");
    password::verify_dummy("a second call hits the cached dummy hash");
}
