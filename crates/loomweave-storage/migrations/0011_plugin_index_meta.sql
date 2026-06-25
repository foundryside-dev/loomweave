-- Migration 0011: per-plugin tag-schema marker for incremental re-analysis
-- (clarion-e12d424f1d).
--
-- The incremental skip keys ONLY on a file's byte content
-- (`file_needs_reanalysis` -> whole-file hash), with no plugin tag-schema
-- component, and the plugin-advertised `ontology_version` was never persisted
-- (handshake-only). So after an operator upgrades a plugin whose emitted
-- vocabulary changed (e.g. the ADR-053/054 reachability-root tags), every
-- UNCHANGED file is silently skipped and keeps its pre-upgrade `entity_tags`
-- rows -- which carry no root tags. The dead-code survey then false-flags the
-- unchanged public surface as dead (the survey's per-plugin honest-empty guard
-- is all-or-nothing and is defeated the moment one re-edited file re-tags).
--
-- This table records the (plugin_version, ontology_version) each plugin last
-- analysed the index under. `analyze` compares the live manifest marker against
-- the stored one and forces a full re-dispatch of that plugin's files when
-- EITHER component moves (or no row exists yet), so an upgrade can never leave
-- a mix of pre- and post-upgrade tag rows. The marker is rewritten in the SAME
-- transaction as the prior-index snapshot, so the two can never disagree after
-- a crash.
--
-- Keyed by `plugin_id` (one row per plugin). A plugin that has never run leaves
-- no row -> treated as "marker absent" -> full re-dispatch (the safe,
-- fail-toward-work direction). Project isolation is by DB file.

-- Wrapped in a single transaction (mirroring 0007) so the CREATE and the
-- migration record commit together; an interruption mid-way must not leave the
-- table in place without the schema_migrations.version=11 row.
BEGIN;

CREATE TABLE plugin_index_meta (
    plugin_id        TEXT PRIMARY KEY,
    plugin_version   TEXT NOT NULL,
    ontology_version TEXT NOT NULL,
    recorded_at      TEXT NOT NULL
);

INSERT INTO schema_migrations (version, name, applied_at)
VALUES (11, '0011_plugin_index_meta', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
