-- Migration 0004 — last-run entity snapshot (prior-index retention).
--
-- Stores the previous successful run's `locator -> body_hash (+ signature)` so
-- (a) incremental analysis can later skip unchanged files/entities, and (b) the
-- Phase-2 SEI matcher can detect vanished locators and compare bodies for the
-- move/rename cases. SHAPE-INDEPENDENT: no SEI column, so this is safe to ship
-- BEFORE SEI lock. The SEI itself lives in a later `sei_bindings` table (the
-- identity source of truth); this table never holds identity.
--
-- The snapshot is REBUILT after each successful run (full replace — see
-- `WriterCmd::UpsertPriorIndex`), so it is always exactly "the last successful
-- run's entities (that carry a body hash)". `signature` is reserved for the
-- WS1 matcher and stays NULL until `entities.signature` exists. Not part of the
-- main entity graph; does not FK into entities (entities is cumulative and
-- never pruned, so a FK with cascade would not model "last run" anyway).

BEGIN;

CREATE TABLE sei_prior_index (
    locator      TEXT    PRIMARY KEY,  -- the entity's full id string (plugin:kind:qualname)
    body_hash    TEXT    NOT NULL,     -- entities.content_hash at prior-run time
    signature    TEXT,                 -- reserved (WS1); NULL until entities.signature exists
    recorded_at  TEXT    NOT NULL      -- ISO-8601 UTC; prior-run completion timestamp
);

-- Record the migration inside the same transaction (defence-in-depth: the
-- runner's INSERT OR IGNORE in apply_one then no-ops). Matches 0001–0003.
INSERT INTO schema_migrations (version, name, applied_at)
VALUES (4, '0004_sei_prior_index', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
