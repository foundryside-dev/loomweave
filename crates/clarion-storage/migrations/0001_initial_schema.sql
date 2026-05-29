-- ============================================================================
-- Clarion migration 0001 — initial schema.
--
-- Source: docs/clarion/1.0/detailed-design.md §3 (Storage Implementation).
-- Sprint 1 walking skeleton writes only to `entities` and `runs`, but every
-- table, FTS5 virtual table, trigger, generated column, and view is created
-- here so the full shape is frozen at L1-lock time. See ADR-011 for the
-- writer-actor + per-N-files transaction model this schema supports.
--
-- Edit-in-place policy (per ADR-024): this migration is editable in place
-- as long as no external operator has produced a `.clarion/clarion.db` from
-- a published Clarion build. The retirement trigger names exactly that
-- condition; once it fires, all schema changes stack as 0002_*.sql etc.
-- The 2026-05-03 edits (guidance vocabulary rename per ADR-024), the
-- 2026-05-18 edits (CHECK constraints on closed-vocabulary TEXT columns per
-- ADR-031), and the 2026-05-24 edit (summary_cache.entity_id FK per
-- V11-STO-03) were all applied under this policy.
-- ============================================================================

BEGIN;

-- Meta: migration tracking. Not in detailed-design §3 — it's the runner's own
-- bookkeeping table. Applied migrations append a row here; re-runs are no-ops.
CREATE TABLE schema_migrations (
    version     INTEGER PRIMARY KEY,
    name        TEXT NOT NULL,
    applied_at  TEXT NOT NULL
);

-- Entities
CREATE TABLE entities (
    id                 TEXT PRIMARY KEY,
    plugin_id          TEXT NOT NULL,
    -- ADR-031: plugin-extensible vocabulary (ADR-022 reserves entity-kind
    -- declaration to the plugin manifest); no CHECK by policy. Writer-actor
    -- + manifest acceptance is the enforcement layer.
    kind               TEXT NOT NULL,
    name               TEXT NOT NULL,
    short_name         TEXT NOT NULL,
    parent_id          TEXT REFERENCES entities(id),
    source_file_id     TEXT REFERENCES entities(id),
    source_file_path   TEXT,
    source_byte_start  INTEGER,
    source_byte_end    INTEGER,
    source_line_start  INTEGER,
    source_line_end    INTEGER,
    properties         TEXT NOT NULL,
    content_hash       TEXT,
    summary            TEXT,
    wardline           TEXT,
    first_seen_commit  TEXT,
    last_seen_commit   TEXT,
    created_at         TEXT NOT NULL,
    updated_at         TEXT NOT NULL
);
CREATE INDEX ix_entities_last_seen_commit ON entities(last_seen_commit);
CREATE INDEX ix_entities_kind              ON entities(kind);
CREATE INDEX ix_entities_plugin_kind       ON entities(plugin_id, kind);
CREATE INDEX ix_entities_parent            ON entities(parent_id);
CREATE INDEX ix_entities_source_file       ON entities(source_file_id);
CREATE INDEX ix_entities_source_file_path  ON entities(source_file_path);
CREATE INDEX ix_entities_content_hash      ON entities(content_hash);

-- Tags (denormalised)
CREATE TABLE entity_tags (
    entity_id  TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    plugin_id  TEXT NOT NULL,
    tag        TEXT NOT NULL,
    PRIMARY KEY (entity_id, plugin_id, tag)
);
CREATE INDEX ix_entity_tags_tag ON entity_tags(tag);
CREATE INDEX ix_entity_tags_plugin_tag ON entity_tags(plugin_id, tag);

-- Edges. Natural PK (kind, from_id, to_id) per ADR-026 decision 4 (B.3).
-- Synthetic `id` column dropped: no Sprint-1 or B.3 query selects edges by
-- `id`; the natural composite is stable across re-analyze, and the only
-- finding-attachment cross-reference (findings.entity_id) points at entities,
-- not edges. The properties bag is evidence/metadata, not identity; callers
-- that need multiple observations for the same relationship merge them into
-- one edge row. WITHOUT ROWID drops the now-redundant rowid pages.
CREATE TABLE edges (
    -- ADR-031: plugin-extensible vocabulary (ADR-022 admits plugin-declared
    -- edge kinds beyond the four core-reserved structural ones); no CHECK by
    -- policy. Writer-actor `enforce_edge_contract` is the enforcement layer
    -- for the v0.1 9-value ontology.
    kind               TEXT NOT NULL,
    from_id            TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    to_id              TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    properties         TEXT,
    source_file_id     TEXT REFERENCES entities(id),
    source_byte_start  INTEGER,
    source_byte_end    INTEGER,
    -- ADR-031 precedent: closed core-owned vocabulary; values per ADR-028.
    confidence         TEXT NOT NULL DEFAULT 'resolved'
                       CHECK (confidence IN ('resolved', 'ambiguous', 'inferred')),
    PRIMARY KEY (kind, from_id, to_id)
) WITHOUT ROWID;
CREATE INDEX ix_edges_from_kind ON edges(from_id, kind);
CREATE INDEX ix_edges_to_kind   ON edges(to_id,   kind);
CREATE INDEX ix_edges_kind      ON edges(kind);
CREATE INDEX ix_edges_kind_confidence ON edges(kind, confidence);

-- Findings
CREATE TABLE findings (
    id                  TEXT PRIMARY KEY,
    tool                TEXT NOT NULL,
    tool_version        TEXT NOT NULL,
    run_id              TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    rule_id             TEXT NOT NULL,
    -- ADR-031: closed core-owned vocabulary; values per ADR-004 +
    -- detailed-design.md §3.
    kind                TEXT NOT NULL
                        CHECK (kind IN ('defect', 'fact', 'classification', 'metric', 'suggestion')),
    -- ADR-031: closed core-owned vocabulary; values per ADR-017 (Clarion
    -- internal severity, pre-mapping to Filigree wire).
    severity            TEXT NOT NULL
                        CHECK (severity IN ('INFO', 'WARN', 'ERROR', 'CRITICAL', 'NONE')),
    confidence          REAL,
    confidence_basis    TEXT,
    entity_id           TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    related_entities    TEXT NOT NULL,
    message             TEXT NOT NULL,
    evidence            TEXT NOT NULL,
    properties          TEXT NOT NULL,
    supports            TEXT NOT NULL,
    supported_by        TEXT NOT NULL,
    -- ADR-031: closed core-owned vocabulary; values per
    -- detailed-design.md §3 finding lifecycle.
    status              TEXT NOT NULL
                        CHECK (status IN ('open', 'acknowledged', 'suppressed', 'promoted_to_issue')),
    suppression_reason  TEXT,
    filigree_issue_id   TEXT,
    created_at          TEXT NOT NULL,
    updated_at          TEXT NOT NULL
);
CREATE INDEX ix_findings_entity    ON findings(entity_id);
CREATE INDEX ix_findings_rule      ON findings(rule_id);
CREATE INDEX ix_findings_tool_rule ON findings(tool, rule_id);
CREATE INDEX ix_findings_run       ON findings(run_id);
CREATE INDEX ix_findings_status    ON findings(status);

-- Summary cache
CREATE TABLE summary_cache (
    -- FK matches the sibling caches (inferred_edge_cache.caller_entity_id,
    -- entity_unresolved_call_sites.caller_entity_id). Added in-place per
    -- ADR-024 on 2026-05-24 (V11-STO-03 closure) — the prior absence was a
    -- bug, not intentional asymmetry. ON DELETE CASCADE so removing an
    -- entity (re-analyze, rename) clears its cached summaries.
    entity_id             TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    content_hash          TEXT NOT NULL,
    prompt_template_id    TEXT NOT NULL,
    model_tier            TEXT NOT NULL,
    guidance_fingerprint  TEXT NOT NULL,
    summary_json          TEXT NOT NULL,
    cost_usd              REAL NOT NULL,
    tokens_input          INTEGER NOT NULL,
    tokens_output         INTEGER NOT NULL,
    created_at            TEXT NOT NULL,
    last_accessed_at      TEXT NOT NULL,
    caller_count          INTEGER NOT NULL,
    fan_out               INTEGER NOT NULL,
    -- ADR-031 precedent: closed core-owned vocabulary; boolean-shaped INTEGER.
    stale_semantic        INTEGER NOT NULL DEFAULT 0 CHECK (stale_semantic IN (0, 1)),
    PRIMARY KEY (entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)
);

-- Inferred edge cache
CREATE TABLE inferred_edge_cache (
    caller_entity_id     TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    caller_content_hash  TEXT NOT NULL,
    model_id             TEXT NOT NULL,
    prompt_version       TEXT NOT NULL,
    result_json          TEXT NOT NULL,
    cost_usd             REAL NOT NULL DEFAULT 0.0,
    token_count          INTEGER NOT NULL DEFAULT 0,
    created_at           TEXT NOT NULL,
    last_accessed_at     TEXT NOT NULL,
    PRIMARY KEY (caller_entity_id, caller_content_hash, model_id, prompt_version)
);

-- Unresolved call sites for query-time inferred dispatch
CREATE TABLE entity_unresolved_call_sites (
    caller_entity_id     TEXT NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    caller_content_hash  TEXT NOT NULL,
    site_key             TEXT NOT NULL,
    site_ordinal         INTEGER NOT NULL,
    source_file_id       TEXT REFERENCES entities(id),
    source_byte_start    INTEGER NOT NULL,
    source_byte_end      INTEGER NOT NULL,
    callee_expr          TEXT NOT NULL,
    created_at           TEXT NOT NULL,
    PRIMARY KEY (caller_entity_id, caller_content_hash, site_key)
);
CREATE INDEX ix_unresolved_call_sites_caller
    ON entity_unresolved_call_sites(caller_entity_id);
CREATE INDEX ix_unresolved_call_sites_expr
    ON entity_unresolved_call_sites(callee_expr);

-- Runs (provenance). Sprint 1 writes started_at/completed_at/config/stats/status;
-- WP2 will populate plugin-invocation fields inside `config` JSON (per UQ-WP1-05).
CREATE TABLE runs (
    id            TEXT PRIMARY KEY,
    started_at    TEXT NOT NULL,
    completed_at  TEXT,
    config        TEXT NOT NULL,
    stats         TEXT NOT NULL,
    -- ADR-031: closed core-owned vocabulary; terminal values from the
    -- `RunStatus` enum (commands.rs); 'running' is the in-flight literal
    -- inserted by BeginRun (writer.rs).
    status        TEXT NOT NULL
                  CHECK (status IN ('running', 'skipped_no_plugins', 'completed', 'failed'))
);

-- FTS5 for text search
CREATE VIRTUAL TABLE entity_fts USING fts5(
    entity_id UNINDEXED,
    name,
    short_name,
    summary_text,
    content_text,
    tokenize = 'porter unicode61'
);

-- FTS5 triggers keep entity_fts synchronised with entities.
CREATE TRIGGER entities_ai AFTER INSERT ON entities BEGIN
    INSERT INTO entity_fts (entity_id, name, short_name, summary_text, content_text)
    VALUES (
        new.id,
        new.name,
        new.short_name,
        COALESCE(json_extract(new.summary, '$.briefing.purpose'), ''),
        ''
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

-- Generated columns + partial indexes for hot JSON properties.
-- scope_level / scope_rank pair (per ADR-024): TEXT for equality filters,
-- INTEGER (CASE-mapped) for ordered queries. The semantic ordering
-- project→subsystem→package→module→class→function is non-lexicographic, so
-- a TEXT-only index cannot serve ORDER BY correctly.
ALTER TABLE entities ADD COLUMN scope_level TEXT
    GENERATED ALWAYS AS (json_extract(properties, '$.scope_level')) VIRTUAL;
ALTER TABLE entities ADD COLUMN scope_rank INTEGER
    GENERATED ALWAYS AS (
        CASE json_extract(properties, '$.scope_level')
            WHEN 'project'   THEN 1
            WHEN 'subsystem' THEN 2
            WHEN 'package'   THEN 3
            WHEN 'module'    THEN 4
            WHEN 'class'     THEN 5
            WHEN 'function'  THEN 6
        END
    ) VIRTUAL;
CREATE INDEX ix_entities_scope_rank ON entities(scope_rank) WHERE scope_rank IS NOT NULL;

ALTER TABLE entities ADD COLUMN git_churn_count INTEGER
    GENERATED ALWAYS AS (json_extract(properties, '$.git_churn_count')) VIRTUAL;
CREATE INDEX ix_entities_churn ON entities(git_churn_count) WHERE git_churn_count IS NOT NULL;

-- briefing_blocked (per ADR-024): a secret-scan-set property that withholds an
-- entity from briefings / federation exposure. Promoting it to a generated
-- column + partial index lets the federation read-API hot path filter blocked
-- entities in SQL instead of parsing every row's properties JSON. NULL when
-- absent (the common case), so the partial index stays small.
ALTER TABLE entities ADD COLUMN briefing_blocked TEXT
    GENERATED ALWAYS AS (json_extract(properties, '$.briefing_blocked')) VIRTUAL;
CREATE INDEX ix_entities_briefing_blocked ON entities(briefing_blocked)
    WHERE briefing_blocked IS NOT NULL;

-- View for guidance resolver. detailed-design.md §3 references a bare `tags`
-- column on `entities` that does not exist under the normalised tag schema;
-- the view aggregates entity_tags via a correlated subquery to produce the
-- same JSON-array row shape the design implies.
CREATE VIEW guidance_sheets AS
SELECT
    e.id,
    e.name,
    json_extract(e.properties, '$.scope_level')          AS scope_level,
    e.scope_rank                                         AS scope_rank,
    json_extract(e.properties, '$.scope.query_types')    AS query_types,
    json_extract(e.properties, '$.scope.token_budget')   AS token_budget,
    json_extract(e.properties, '$.match_rules')          AS match_rules,
    json_extract(e.properties, '$.content')              AS content,
    json_extract(e.properties, '$.expires')              AS expires,
    json_extract(e.properties, '$.pinned')               AS pinned,
    json_extract(e.properties, '$.provenance')           AS provenance,
    (
        SELECT json_group_array(tag)
        FROM entity_tags
        WHERE entity_id = e.id
    )                                                     AS tags
FROM entities e
WHERE e.kind = 'guidance';

-- Record the migration.
INSERT INTO schema_migrations (version, name, applied_at)
VALUES (1, '0001_initial_schema', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

COMMIT;
