-- Migration 0009: drop the dead entity_fts.content_text column (V11-STO-06,
-- clarion-716449c371).
--
-- content_text shipped in 0001 reserved for an on-demand source-text projection
-- that was never implemented: the entities_ai trigger always wrote '', the
-- entities_au trigger never touched it, and no query reads it (search MATCHes
-- the table, not the column). Semantic/content search is instead served by the
-- ADR-040 embeddings sidecar, so the column is permanently-empty drift that
-- misrepresents the FTS surface. FTS5 has no ALTER ... DROP COLUMN, so recreate
-- the virtual table and its triggers without it. Behaviour-preserving: only a
-- never-populated, never-read column is removed.

BEGIN;

DROP TRIGGER IF EXISTS entities_ai;
DROP TRIGGER IF EXISTS entities_au;
DROP TRIGGER IF EXISTS entities_ad;
DROP TABLE IF EXISTS entity_fts;

CREATE VIRTUAL TABLE entity_fts USING fts5(
    entity_id UNINDEXED,
    name,
    short_name,
    summary_text,
    tokenize = 'porter unicode61'
);

-- FTS5 triggers keep entity_fts synchronised with entities (content_text dropped).
CREATE TRIGGER entities_ai AFTER INSERT ON entities BEGIN
    INSERT INTO entity_fts (entity_id, name, short_name, summary_text)
    VALUES (
        new.id,
        new.name,
        new.short_name,
        COALESCE(json_extract(new.summary, '$.briefing.purpose'), '')
    );
END;
CREATE TRIGGER entities_au AFTER UPDATE ON entities BEGIN
    UPDATE entity_fts
    SET name         = new.name,
        short_name   = new.short_name,
        summary_text = COALESCE(json_extract(new.summary, '$.briefing.purpose'), '')
    WHERE entity_id = new.id;
END;
CREATE TRIGGER entities_ad AFTER DELETE ON entities BEGIN
    DELETE FROM entity_fts WHERE entity_id = old.id;
END;

-- Rebuild the index from existing entities (the recreated vtable starts empty).
INSERT INTO entity_fts (entity_id, name, short_name, summary_text)
SELECT id, name, short_name,
       COALESCE(json_extract(summary, '$.briefing.purpose'), '')
FROM entities;

INSERT INTO schema_migrations (version, name, applied_at)
VALUES (9, '0009_drop_fts_content_text', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
