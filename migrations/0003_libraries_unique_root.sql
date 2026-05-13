-- A library is identified by its root path; two libraries pointing at the
-- same directory would cause double-indexing during scans. Enforce
-- uniqueness at the schema layer so the API doesn't have to TOCTOU-check.
-- SQLite doesn't support ALTER TABLE ADD UNIQUE; CREATE UNIQUE INDEX
-- gives the same guarantee without a table rebuild.

CREATE UNIQUE INDEX idx_libraries_root_path ON libraries(root_path);
