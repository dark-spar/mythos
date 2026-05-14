-- Phase 5c schema: subtitle tracks discovered per file.
--
-- ffprobe enumerates these during the scan. The serving path treats
-- text subs (SRT/ASS/SSA/mov_text/...) and image subs (PGS/VOBSUB/...)
-- differently:
--   * text subs are extracted to WebVTT on demand and served as a
--     <track> sidecar to the <video> element.
--   * image subs have to be burned into the transcoded video stream;
--     `is_image` is the flag the API uses to route between those.
--
-- UNIQUE (file_id, stream_index) so rescans replace cleanly rather
-- than accumulating duplicates.

CREATE TABLE media_subtitles (
    id            TEXT PRIMARY KEY NOT NULL,                                   -- uuid v7
    file_id       TEXT NOT NULL REFERENCES media_files(id) ON DELETE CASCADE,
    stream_index  INTEGER NOT NULL,                                            -- ffprobe stream index, used by `-map 0:N`
    codec         TEXT NOT NULL,                                                -- e.g. "subrip", "ass", "hdmv_pgs_subtitle", "dvd_subtitle"
    language      TEXT,                                                         -- ISO 639-2/B from stream tag (e.g. "eng")
    title         TEXT,                                                         -- ffprobe stream title tag
    is_image      INTEGER NOT NULL DEFAULT 0,                                   -- 1 = bitmap (PGS/VOBSUB/DVB), 0 = text
    is_default    INTEGER NOT NULL DEFAULT 0,                                   -- disposition.default
    is_forced     INTEGER NOT NULL DEFAULT 0,                                   -- disposition.forced
    UNIQUE (file_id, stream_index)
);

CREATE INDEX idx_media_subtitles_file ON media_subtitles(file_id);
