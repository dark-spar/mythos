-- Initial schema for Mythos.
-- Phase 0 ships only what the server needs to boot:
--   * users (auth lands in Phase 1, but the table is here so migrations are append-only)
--   * libraries (root paths the scanner will walk)
-- Per-media-kind tables are added in subsequent migrations.

PRAGMA foreign_keys = ON;

CREATE TABLE users (
    id            TEXT PRIMARY KEY NOT NULL,                       -- uuid v7
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,                                    -- argon2id
    is_admin      INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE libraries (
    id           TEXT PRIMARY KEY NOT NULL,                         -- uuid v7
    name         TEXT NOT NULL,
    kind         TEXT NOT NULL CHECK (kind IN ('movies','shows','music','photos','books')),
    root_path    TEXT NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_libraries_kind ON libraries(kind);
