-- Migration 0002: briefing_blocked generated column + partial index (ADR-024).
--
-- Originally added in place to 0001, which meant databases already stamped at
-- schema_migrations.version=1 never received the column — project_status then
-- hit `no such column: entities.briefing_blocked`. Shipping it as a distinct
-- migration lets the runner apply it to existing v1 databases on upgrade.
--
-- briefing_blocked is a secret-scan-set property that withholds an entity from
-- briefings / federation exposure. Promoting it to a generated column + partial
-- index lets the federation read-API hot path filter blocked entities in SQL
-- instead of parsing every row's properties JSON. NULL when absent (the common
-- case), so the partial index stays small.

-- Wrapped in a single transaction (mirroring 0001) so the ALTER, the index,
-- and the migration record commit together. Without this, an interruption
-- after the ALTER but before the version row is written leaves the column in
-- place with no schema_migrations.version=2 row; the next startup reruns the
-- ALTER and dies on a duplicate-column error, blocking upgrade.
BEGIN;

ALTER TABLE entities ADD COLUMN briefing_blocked TEXT
    GENERATED ALWAYS AS (json_extract(properties, '$.briefing_blocked')) VIRTUAL;
CREATE INDEX ix_entities_briefing_blocked ON entities(briefing_blocked)
    WHERE briefing_blocked IS NOT NULL;

-- Record the migration inside the same transaction (defence-in-depth: the
-- runner's INSERT OR IGNORE in apply_one then no-ops). Matches 0001.
INSERT INTO schema_migrations (version, name, applied_at)
VALUES (2, '0002_briefing_blocked', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
