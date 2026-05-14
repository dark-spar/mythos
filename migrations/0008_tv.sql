-- Phase 3a schema: TV series → seasons → episodes, plus per-user
-- episode progress.
--
-- Mirrors the movies / media_files relationship: an `episodes` row FKs
-- 1:1 to a `media_files` row, just like `movies` does, so subtitles,
-- ffprobe data, byte-range streaming, HLS transcoding, and the prune
-- pass all work for episodes without any changes to existing code.
--
-- Series identity before TMDb enrichment is (library_id, sort_title);
-- a re-scan never duplicates a series even if its TMDb ID is still
-- NULL. Enrichment fills in tmdb_id / overview / poster_url without
-- changing the natural key.
--
-- Cascade behavior: deleting a library nukes its media_files (existing
-- behavior), which nukes the episode rows that reference them. Empty
-- season / series rows left behind after the prune pass are cleaned
-- up by the scanner before it returns (parent rows have no FK back to
-- episodes, so they wouldn't cascade automatically).

CREATE TABLE series (
    id          TEXT PRIMARY KEY NOT NULL,                                       -- uuid v7
    library_id  TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    title       TEXT NOT NULL,
    sort_title  TEXT NOT NULL,
    year        INTEGER,                                                          -- first-air year
    tmdb_id     INTEGER,
    overview    TEXT,
    poster_url  TEXT,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (library_id, sort_title)
);

CREATE INDEX idx_series_library    ON series(library_id);
CREATE INDEX idx_series_sort_title ON series(sort_title COLLATE NOCASE);

CREATE TABLE seasons (
    id             TEXT PRIMARY KEY NOT NULL,                                    -- uuid v7
    series_id      TEXT NOT NULL REFERENCES series(id) ON DELETE CASCADE,
    season_number  INTEGER NOT NULL,                                             -- 0 = specials
    title          TEXT,
    tmdb_id        INTEGER,
    overview       TEXT,
    poster_url     TEXT,
    created_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (series_id, season_number)
);

CREATE INDEX idx_seasons_series ON seasons(series_id);

CREATE TABLE episodes (
    id              TEXT PRIMARY KEY NOT NULL,                                   -- uuid v7
    season_id       TEXT NOT NULL REFERENCES seasons(id)     ON DELETE CASCADE,
    file_id         TEXT NOT NULL UNIQUE
                    REFERENCES media_files(id) ON DELETE CASCADE,
    episode_number  INTEGER NOT NULL,
    title           TEXT,
    tmdb_id         INTEGER,
    overview        TEXT,
    still_url       TEXT,
    air_date        TEXT,                                                         -- ISO-8601 yyyy-mm-dd
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (season_id, episode_number)
);

CREATE INDEX idx_episodes_season ON episodes(season_id);

CREATE TABLE episode_progress (
    user_id           TEXT NOT NULL REFERENCES users(id)    ON DELETE CASCADE,
    episode_id        TEXT NOT NULL REFERENCES episodes(id) ON DELETE CASCADE,
    position_seconds  REAL NOT NULL,
    duration_seconds  REAL NOT NULL,
    updated_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (user_id, episode_id)
);

CREATE INDEX idx_episode_progress_user ON episode_progress(user_id);
