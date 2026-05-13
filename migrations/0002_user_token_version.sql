-- Per-user token version. Bumped on logout / password change / admin revoke
-- to invalidate any outstanding JWTs, since otherwise a stolen bearer token
-- is valid until expiry. The auth extractor compares the JWT's `ver` claim
-- against this column on every authenticated request.

ALTER TABLE users ADD COLUMN token_version INTEGER NOT NULL DEFAULT 0;
