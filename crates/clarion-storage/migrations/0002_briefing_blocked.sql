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

ALTER TABLE entities ADD COLUMN briefing_blocked TEXT
    GENERATED ALWAYS AS (json_extract(properties, '$.briefing_blocked')) VIRTUAL;
CREATE INDEX ix_entities_briefing_blocked ON entities(briefing_blocked)
    WHERE briefing_blocked IS NOT NULL;
