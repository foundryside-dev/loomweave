-- Migration 0005 — SEI identity store + lineage event log (Wave 1 / WS1).
--
-- Implements ADR-038 (token scheme, signature schema, identity persistence) and
-- the Weft SEI conformance standard §3 (matcher state) / §4 (resolution surface).
--
-- sei_bindings:       the durable identity store, keyed by the opaque SEI. It is
--                     DECOUPLED from the cumulative `entities` table (which is
--                     never pruned — `writer.rs` upserts ON CONFLICT(id) and there
--                     is no DELETE on re-index), so carrying an SEI across a rename
--                     can never collide with the stale entity row that still holds
--                     the old locator. Orphaning is a `status` flip, never a row
--                     deletion. There is deliberately NO `entities.sei` column: a
--                     UNIQUE sei on a cumulative table breaks the rename-carry.
-- entities.signature: plugin-declared, versioned JSON, stored verbatim. PLAIN
--                     TEXT — not unique (signatures are shared across overloads /
--                     identical shapes), compared by string equality. Near-redundant
--                     for the v1 deterministic move case (a byte-identical body
--                     already implies an identical signature line); carried for
--                     SEI-spec §3 move-predicate conformance and as the load-bearing
--                     input to the North-Star fuzzy matcher.
-- sei_lineage:        append-only identity-event log (REQ-L-01 — INSERT only, no
--                     UPDATE path; consumer/legis re-establishes integrity at its
--                     own boundary in v1).

BEGIN;

ALTER TABLE entities ADD COLUMN signature TEXT;

CREATE TABLE sei_bindings (
    sei             TEXT    PRIMARY KEY,   -- loomweave:eid:<hex> (opaque; consumers MUST NOT parse)
    current_locator TEXT,                  -- current address: the alive binding's entity id
    body_hash       TEXT,                  -- entities.content_hash at last (re)bind
    signature       TEXT,                  -- entities.signature at last (re)bind
    status          TEXT    NOT NULL CHECK(status IN ('alive','orphaned','superseded')),
    born_run_id     TEXT    NOT NULL,      -- mint_run_id: the run that first minted this SEI
    updated_run_id  TEXT    NOT NULL,      -- run that last carried/updated this binding
    updated_at      TEXT    NOT NULL        -- ISO-8601 UTC
);

-- At most ONE alive binding per locator. Partial unique index — orphaned/superseded
-- bindings may share a former locator without colliding (audit history is retained).
CREATE UNIQUE INDEX ux_sei_alive_locator
    ON sei_bindings(current_locator)
    WHERE status = 'alive' AND current_locator IS NOT NULL;

CREATE TABLE sei_lineage (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    sei          TEXT    NOT NULL,
    event        TEXT    NOT NULL CHECK(event IN
                     ('born','locator_changed','moved','orphaned','superseded')),
    old_locator  TEXT,            -- set for locator_changed, moved, orphaned
    new_locator  TEXT,            -- set for born, locator_changed, moved
    run_id       TEXT    NOT NULL,
    recorded_at  TEXT    NOT NULL  -- ISO-8601 UTC
);

CREATE INDEX ix_sei_lineage_sei ON sei_lineage(sei);

-- Record the migration inside the same transaction (defence-in-depth: the
-- runner's INSERT OR IGNORE in apply_one then no-ops). Matches 0001–0004.
INSERT INTO schema_migrations (version, name, applied_at)
VALUES (5, '0005_sei', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
