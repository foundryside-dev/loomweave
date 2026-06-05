-- Migration 0006: SEI lookup key on the Wardline taint store (T3.4).
--
-- Additive, no primary-key change. `wardline_taint_facts` stays locator-keyed
-- (`entity_id` PRIMARY KEY → entities.id, migration 0003); this adds a SECOND,
-- rename-stable lookup key so a fact written under a former locator is still
-- retrievable after the entity is renamed.
--
-- Why this is safe to ship BEFORE the suite-wide SEI cutover: the column is
-- nullable and populated lazily at write time (the write path resolves the
-- alive `sei_bindings` row for the fact's locator, or leaves it NULL on a
-- pre-SEI database). No backfill of existing rows; facts stay physically
-- anchored to their `entity_id` (the never-pruned `entities` table preserves
-- it). The SEI is opaque to the store — written and matched verbatim, never
-- parsed (ADR-038 / Weft SEI standard §4).
--
-- Partial index (WHERE sei IS NOT NULL): the read-by-SEI lookup filters on
-- non-null SEIs only, and a partial index keeps the pre-SEI NULL rows out of
-- the b-tree.

BEGIN;

ALTER TABLE wardline_taint_facts ADD COLUMN sei TEXT;

CREATE INDEX ix_wardline_taint_sei
    ON wardline_taint_facts(sei)
    WHERE sei IS NOT NULL;

-- Record the migration inside the same transaction (defence-in-depth: the
-- runner's INSERT OR IGNORE in apply_one then no-ops). Matches 0001–0005.
INSERT INTO schema_migrations (version, name, applied_at)
VALUES (6, '0006_wardline_taint_sei', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
