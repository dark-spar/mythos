-- Color/HDR metadata captured from ffprobe.
--
-- Lets the transcoder decide whether to apply HDR→SDR tonemapping
-- without re-probing the source on every segment request. All three
-- columns mirror the ffprobe stream fields verbatim
-- (`color_primaries`, `color_transfer`, `color_space`) so the values
-- are interpretable later without reverse-engineering our own enum.
--
-- Existing rows stay NULL until a rescan refills them — the
-- tonemap-decision code treats NULL as "assume SDR", which is the
-- right fallback for libraries scanned before this migration.

ALTER TABLE media_files ADD COLUMN color_primaries TEXT;
ALTER TABLE media_files ADD COLUMN color_transfer  TEXT;
ALTER TABLE media_files ADD COLUMN color_space     TEXT;
