-- Phase 1c schema: movies + the media_files they live in.
--
-- `path` is relative to the owning library's root, so operators can
-- relocate a library by changing libraries.root_path without rewriting
-- every row. Streaming code concatenates root_path + path at serve time.
--
-- Nullable technical columns are intentional: ffprobe may be unavailable
-- or fail on individual files; the scan is "tolerant" and indexes the
-- file anyway with NULL fields. A future re-scan fills them in.
--
-- ON DELETE CASCADE on both FKs: deleting a library nukes its
-- media_files, which in turn nukes the movies that reference them.

CREATE TABLE media_files (
    id                TEXT PRIMARY KEY NOT NULL,                                 -- uuid v7
    library_id        TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    path              TEXT NOT NULL,                                             -- relative to library.root_path
    size_bytes        INTEGER NOT NULL,
    mtime             TEXT NOT NULL,                                             -- RFC3339
    container         TEXT,
    video_codec       TEXT,
    audio_codec       TEXT,
    duration_seconds  REAL,
    width             INTEGER,
    height            INTEGER,
    scanned_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (library_id, path)
);

CREATE INDEX idx_media_files_library ON media_files(library_id);

CREATE TABLE movies (
    id          TEXT PRIMARY KEY NOT NULL,                                       -- uuid v7
    library_id  TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
    file_id     TEXT NOT NULL UNIQUE REFERENCES media_files(id) ON DELETE CASCADE,
    title       TEXT NOT NULL,
    sort_title  TEXT NOT NULL,
    year        INTEGER,
    tmdb_id     INTEGER,                                                          -- filled in Phase 1d
    overview    TEXT,                                                             -- filled in Phase 1d
    poster_url  TEXT,                                                             -- filled in Phase 1d
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX idx_movies_library ON movies(library_id);
CREATE INDEX idx_movies_sort_title ON movies(sort_title COLLATE NOCASE);
