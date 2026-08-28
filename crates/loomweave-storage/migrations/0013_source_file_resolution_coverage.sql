-- Migration 0013: per-file call / reference resolution coverage
-- (clarion-3e517d4aff).
--
-- The Python plugin degrades to EMPTY call / reference evidence -- not an
-- error -- whenever its language server times out, crashes, or is poisoned
-- for the rest of the run. The host persisted that as a completed analysis:
-- the file's prior anchored edges were replaced by nothing, its whole-file
-- hash landed in the prior index, and the incremental skip then treated the
-- byte-identical file as done on every later run. A transient resolver
-- failure became a permanent hole in the call graph, and the read surface
-- reported `traversal_complete: true` over it.
--
-- This table records, per analysed source file, the coverage the plugin
-- CLAIMED for each resolution facet. `analyze` force-re-dispatches every
-- `degraded && transient` file on incremental runs (a byte-identical file is
-- no longer skippable until a run reports it complete), the MCP caller
-- navigation names a degraded index in `scope_excludes`, and `doctor`
-- surfaces the count. A plugin that makes no coverage claim (a purely
-- syntactic extractor) is recorded as complete.
--
-- Keyed by the core `file` entity id. Rows are replaced in the same per-file
-- transaction as the file's anchored edges and dropped with them when the
-- file vanishes from disk. Project isolation is by DB file.

BEGIN;

CREATE TABLE source_file_resolution_coverage (
    source_file_id        TEXT PRIMARY KEY,
    calls_status          TEXT NOT NULL CHECK (calls_status IN ('complete', 'degraded')),
    calls_reason          TEXT,
    calls_transient       INTEGER NOT NULL DEFAULT 0 CHECK (calls_transient IN (0, 1)),
    references_status     TEXT NOT NULL CHECK (references_status IN ('complete', 'degraded')),
    references_reason     TEXT,
    references_transient  INTEGER NOT NULL DEFAULT 0 CHECK (references_transient IN (0, 1)),
    run_id                TEXT NOT NULL,
    updated_at            TEXT NOT NULL
);

CREATE INDEX idx_source_file_resolution_coverage_degraded
    ON source_file_resolution_coverage (calls_status, references_status);

INSERT INTO schema_migrations (version, name, applied_at)
VALUES (13, '0013_source_file_resolution_coverage', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
