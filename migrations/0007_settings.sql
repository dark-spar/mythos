-- Key/value app settings configurable from the admin UI.
--
-- Stays generic so we can park additional settings here later
-- (download paths, scan schedule, etc.) without another schema
-- migration. `updated_at` is informational only, not a load-bearing
-- column.
--
-- Environment variables take precedence over rows in this table —
-- see `resolve_tmdb_api_key` in mythos-api. That keeps the operator
-- pattern of "set MYTHOS_TMDB_API_KEY in the systemd unit and
-- you're done" working alongside the new in-browser configuration.

CREATE TABLE settings (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
