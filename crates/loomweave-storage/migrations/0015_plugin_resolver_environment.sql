-- Migration 0015: resolver-environment fingerprint per plugin (clarion-5cf9643de9).
--
-- The Python plugin's call/reference evidence depends on which interpreter
-- pyright resolved against. `analyze` now records the host-discovered
-- interpreter fingerprint here and forces a full re-dispatch of the plugin's
-- files when it changes, exactly as for a plugin/ontology version bump.
-- NULL = never recorded (pre-migration rows) -> treated as changed, so the
-- first run after upgrade re-dispatches once and heals rows pinned by a
-- launcher-dependent interpreter. Non-language-server plugins keep NULL.

BEGIN;

ALTER TABLE plugin_index_meta
ADD COLUMN resolver_environment TEXT;

INSERT INTO schema_migrations (version, name, applied_at)
VALUES (
    15,
    '0015_plugin_resolver_environment',
    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
);

COMMIT;
