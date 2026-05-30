-- Migration 0003: Wardline taint-fact store (SP9, ADR-036).
-- Dedicated, Wardline-owned per-entity table. NOT the schema-reserved
-- `entities.wardline` column (which `analyze` clobbers with NULL on every
-- re-index). `wardline_json` is opaque to Clarion — stored and returned
-- verbatim. `scan_id` and `content_hash_at_compute` are queryable columns
-- supplied by the caller, not parsed out of the blob.

BEGIN;

CREATE TABLE wardline_taint_facts (
    entity_id               TEXT PRIMARY KEY
                                 REFERENCES entities(id) ON DELETE CASCADE,
    wardline_json           TEXT NOT NULL,
    scan_id                 TEXT,
    content_hash_at_compute TEXT,
    updated_at              TEXT NOT NULL
);

-- Record the migration inside the same transaction (defence-in-depth: the
-- runner's INSERT OR IGNORE in apply_one then no-ops). Matches 0001 and 0002.
INSERT INTO schema_migrations (version, name, applied_at)
VALUES (3, '0003_wardline_taint_facts', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
