-- Migration 0012: version host-owned finding identity per plugin.
--
-- A run-level marker cannot represent a plugin that was absent from that run:
-- its older plugin_index_meta row survives and must still force a full
-- redispatch when the plugin returns. Existing rows default to contract 0 so
-- every pre-migration plugin receives the v2 syntax-finding identity repair.

BEGIN;

ALTER TABLE plugin_index_meta
ADD COLUMN host_syntax_finding_contract INTEGER NOT NULL DEFAULT 0
    CHECK (host_syntax_finding_contract >= 0);

INSERT INTO schema_migrations (version, name, applied_at)
VALUES (
    12,
    '0012_plugin_syntax_finding_contract',
    strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
);

COMMIT;
