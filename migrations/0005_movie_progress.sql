-- Per-user resume points for movie playback.
--
-- Phase 3 will add episode_progress / track_progress (or a polymorphic
-- "items" table — TBD when that phase lands). For now we keep this
-- kind-specific so FKs cascade cleanly: deleting a user or a movie
-- removes the corresponding progress row without polymorphic
-- machinery.
--
-- duration_seconds is stored alongside position_seconds so the SPA can
-- render "12:34 / 1:45:00" without re-probing the file, and so
-- "continue watching" filters in later phases (Phase 3+) can do
-- position/duration comparisons against the row alone.

CREATE TABLE movie_progress (
    user_id          TEXT NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
    movie_id         TEXT NOT NULL REFERENCES movies(id) ON DELETE CASCADE,
    position_seconds REAL NOT NULL,
    duration_seconds REAL NOT NULL,
    updated_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    PRIMARY KEY (user_id, movie_id)
);

CREATE INDEX idx_movie_progress_user ON movie_progress(user_id);
