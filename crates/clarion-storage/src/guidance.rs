//! Guidance-sheet write API (WS6 / REQ-GUIDANCE-01, REQ-GUIDANCE-03).
//!
//! Guidance sheets are entities with `kind = 'guidance'` and id
//! `core:guidance:<slug>`. They are operator-authored, have **no source file**,
//! and exist **outside any `clarion analyze` run** — so they must NOT go through
//! the run-scoped writer actor (`WriterCmd::InsertEntity`), which hard-requires
//! a `BeginRun` and a source-file anchor. Instead they insert via a plain,
//! non-run-scoped `INSERT INTO entities`, exactly the shape proven by the
//! storage schema test `entity_generated_columns_extract_from_properties_json`.
//!
//! The `properties` JSON this module writes is the contract the read path
//! (`clarion-mcp` `catalogue::inspection::tool_guidance_for` / `rule_match`)
//! consumes. In particular `match_rules` entries are `{"type": …, …}` objects:
//!   - `{"type":"path","pattern":"<glob>"}`
//!   - `{"type":"tag","value":"<tag>"}`
//!   - `{"type":"kind","value":"<entity-kind>"}`
//!   - `{"type":"subsystem","id":"<subsystem-id>"}`
//!   - `{"type":"entity","id":"<entity-id>"}`
//!
//! Never set the generated columns (`scope_level`, `scope_rank`,
//! `git_churn_count`) directly — they extract from `properties` JSON via the
//! migration's `GENERATED ALWAYS AS` definitions; `scope_rank` is a CASE-mapped
//! VIRTUAL column (project→1 … function→6).

use std::collections::HashSet;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

use crate::glob::glob_match;
use crate::query::{EntityRow, entity_by_id, subsystem_of_entity};
use crate::{Result, StorageError};

/// The fully-resolved write payload for one guidance sheet. The caller (the CLI)
/// builds this from `--match` / `--scope-level` / `--content` / … and hands it
/// to [`upsert_guidance_sheet`]. `properties_json` is the verbatim object stored
/// in `entities.properties`; this module is the single place that knows the
/// column layout, but the caller owns the JSON shape so it can round-trip an
/// edited sheet without losing fields it does not understand.
pub struct GuidanceSheetInput<'a> {
    /// Full entity id: `core:guidance:<slug>`.
    pub id: &'a str,
    /// `entities.name` — the canonical qualified name (segment 3 of the id).
    pub name: &'a str,
    /// `entities.short_name` — display tail of the name.
    pub short_name: &'a str,
    /// The complete `properties` JSON object (must include at least `content`,
    /// `scope_level`, `provenance`, `authored_at`).
    pub properties: &'a Value,
}

/// A guidance sheet read back from storage. `properties` is the parsed
/// `entities.properties` object; `scope_rank` is the generated column so callers
/// can order without re-deriving the CASE map.
#[derive(Debug, Clone)]
pub struct GuidanceSheet {
    pub id: String,
    pub name: String,
    pub short_name: String,
    pub scope_level: Option<String>,
    pub scope_rank: Option<i64>,
    pub properties: Value,
    pub created_at: String,
    pub updated_at: String,
}

impl GuidanceSheet {
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        let properties_raw: String = row.get(4)?;
        let properties = serde_json::from_str::<Value>(&properties_raw)
            .unwrap_or_else(|_| json!({ "_raw": properties_raw }));
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            short_name: row.get(2)?,
            scope_level: row.get::<_, Option<String>>(3)?,
            scope_rank: row.get::<_, Option<i64>>(5)?,
            properties,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
        })
    }

    /// `$.authored_at` from properties, used for tie-break ordering to mirror
    /// the read path's `scope_rank ASC, authored_at ASC, id ASC`.
    fn authored_at(&self) -> Option<&str> {
        self.properties.get("authored_at").and_then(Value::as_str)
    }
}

const SELECT_COLUMNS: &str = "id, name, short_name, scope_level, properties, \
     scope_rank, created_at, updated_at";

/// Insert or replace a guidance sheet. On a fresh id this inserts; on an
/// existing id it updates `name`, `short_name`, `properties`, and bumps
/// `updated_at` (preserving `created_at`). The generated columns recompute from
/// the new `properties` automatically.
///
/// This is the low-level overwrite primitive. The CLI's `create` guards against
/// clobbering an existing id (that is `edit`'s job); `edit` does a
/// read-modify-write that preserves `authored_at` / `provenance` / `pinned`.
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] on any `SQLite` failure (lock, constraint).
pub fn upsert_guidance_sheet(conn: &Connection, sheet: &GuidanceSheetInput<'_>) -> Result<()> {
    let properties = serde_json::to_string(sheet.properties)
        .map_err(|e| StorageError::InvalidQuery(format!("serialize guidance properties: {e}")))?;
    conn.execute(
        "INSERT INTO entities \
            (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
         VALUES \
            (?1, 'core', 'guidance', ?2, ?3, ?4, \
             strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')) \
         ON CONFLICT(id) DO UPDATE SET \
            name = excluded.name, \
            short_name = excluded.short_name, \
            properties = excluded.properties, \
            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        params![sheet.id, sheet.name, sheet.short_name, properties],
    )?;
    Ok(())
}

/// Fetch one guidance sheet by id. Returns `None` if the id is absent or the
/// row exists but is not `kind = 'guidance'`.
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] on any `SQLite` failure.
pub fn get_guidance_sheet(conn: &Connection, id: &str) -> Result<Option<GuidanceSheet>> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM entities WHERE id = ?1 AND kind = 'guidance'");
    let sheet = conn
        .query_row(&sql, params![id], GuidanceSheet::from_row)
        .optional()?;
    Ok(sheet)
}

/// List guidance sheets, ordered to mirror the read path's composition sort:
/// `scope_rank ASC` (NULLs last), then `authored_at ASC`, then `id ASC`. So
/// CLI `list` output and `guidance_for` composition agree on ordering.
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] on any `SQLite` failure.
pub fn list_guidance_sheets(conn: &Connection) -> Result<Vec<GuidanceSheet>> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM entities WHERE kind = 'guidance'");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], GuidanceSheet::from_row)?;
    let mut sheets: Vec<GuidanceSheet> = rows.collect::<rusqlite::Result<_>>()?;
    sheets.sort_by(|a, b| {
        a.scope_rank
            .unwrap_or(i64::MAX)
            .cmp(&b.scope_rank.unwrap_or(i64::MAX))
            .then_with(|| a.authored_at().cmp(&b.authored_at()))
            .then_with(|| a.id.cmp(&b.id))
    });
    Ok(sheets)
}

/// Delete one guidance sheet by id. Returns `true` if a `kind = 'guidance'` row
/// was removed, `false` if no such sheet existed.
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] on any `SQLite` failure.
pub fn delete_guidance_sheet(conn: &Connection, id: &str) -> Result<bool> {
    let affected = conn.execute(
        "DELETE FROM entities WHERE id = ?1 AND kind = 'guidance'",
        params![id],
    )?;
    Ok(affected > 0)
}

/// True if `sheet` applies to the entity `entity_id`, evaluating its
/// `match_rules` (path / tag / kind / subsystem / entity) against the entity's
/// facts. Uses the shared [`rule_match`] dispatch, so CLI `list --for-entity`
/// stays consistent with the MCP `guidance_for` read path on rule semantics.
///
/// This considers **`match_rules` only** — it deliberately ignores explicit
/// `guides`-edge composition (which `guidance_for` *also* honours), so a `true`
/// here means "a match rule fired", not "`guidance_for` would compose this sheet".
/// `wardline_group` rules are not evaluable here and never match (the Wardline
/// blob is opaque to Clarion).
///
/// `project_root` is needed to compute the entity's project-relative path for
/// `path` rules (the stored `source_file_path` is absolute).
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] on any `SQLite` failure resolving entity facts.
pub fn guidance_sheet_matches_entity(
    conn: &Connection,
    sheet: &GuidanceSheet,
    entity_id: &str,
    project_root: &Path,
) -> Result<bool> {
    let Some(rules) = sheet
        .properties
        .get("match_rules")
        .and_then(Value::as_array)
    else {
        return Ok(false);
    };
    if rules.is_empty() {
        return Ok(false);
    }
    let Some(facts) = MatchFacts::from_entity_id(conn, entity_id, project_root)? else {
        return Ok(false);
    };
    Ok(rules
        .iter()
        .any(|rule| matches!(rule_match(rule, &facts), RuleVerdict::Matched(_))))
}

/// Invalidate (delete) cached summaries for every entity `sheet` matches,
/// returning the number of `summary_cache` rows removed (WS6 / T-cache,
/// ADR-007).
///
/// This is the eager-invalidation half of ADR-007's guidance contract: a
/// guidance-sheet edit changes the composed guidance, so the cached summaries of
/// every affected entity must be dropped or the new guidance never reaches a
/// future prompt (it would otherwise stay inert until the entity's *code*
/// changed and its `content_hash` cache key rotated). The CLI authoring path
/// (`clarion guidance create|edit|delete`) calls this on every mutation.
///
/// Scan strategy: drive off `SELECT DISTINCT entity_id FROM summary_cache` (the
/// only entities that *can* be invalidated), not the whole entity table — this
/// keeps the work O(cached-entities) ≤ O(N-entities) and, by reusing
/// [`delete_summary_cache_for_entity`]'s single-entity `DELETE`, dodges the
/// `SQLite` 999-bound-parameter ceiling a broad `IN (…)` over a wide `path:`
/// match would otherwise hit on a large corpus. Guidance sheets never carry
/// cache rows, so the `kind = 'guidance'` exclusion is automatic.
///
/// A sheet with no (evaluable) `match_rules` matches nothing and this is a clean
/// 0-row no-op — that is correct: explicit `guides`-edge composition is handled
/// elsewhere (analyze.rs deletion path) and is deliberately out of scope here.
///
/// `project_root` is required to evaluate `path:` rules (the stored
/// `source_file_path` is absolute; the matcher strips this prefix to a
/// project-relative path). It is canonicalized to align with symlink-resolved
/// stored paths, mirroring the CLI `list` path.
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] on any `SQLite` failure enumerating cached
/// entities, resolving entity facts, or deleting rows.
pub fn invalidate_summaries_for_sheet(
    conn: &Connection,
    sheet: &GuidanceSheet,
    project_root: &Path,
) -> Result<usize> {
    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    let cached_ids: Vec<String> = {
        let mut stmt = conn.prepare("SELECT DISTINCT entity_id FROM summary_cache")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let mut removed = 0usize;
    for entity_id in &cached_ids {
        if guidance_sheet_matches_entity(conn, sheet, entity_id, &canonical_root)? {
            removed += crate::cache::delete_summary_cache_for_entity(conn, entity_id)?;
        }
    }
    Ok(removed)
}

/// The minimum entity facts a guidance `match_rules` evaluation needs. This is
/// the single source of truth shared by the CLI write path
/// ([`guidance_sheet_matches_entity`]) and the MCP `guidance_for` read path —
/// the read path builds one from an already-loaded `EntityRow`
/// ([`MatchFacts::from_entity_row`]) to avoid a second lookup; the CLI resolves
/// by id ([`MatchFacts::from_entity_id`]).
pub struct MatchFacts {
    kind: String,
    rel_path: Option<String>,
    tags: HashSet<String>,
    subsystem_id: Option<String>,
    entity_id: String,
}

impl MatchFacts {
    /// Build facts from an already-loaded [`EntityRow`] (the read path has the
    /// row in hand for the response, so it should not re-query).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] on any `SQLite` failure loading tags or
    /// the entity's subsystem.
    pub fn from_entity_row(
        conn: &Connection,
        entity: &EntityRow,
        project_root: &Path,
    ) -> Result<Self> {
        let rel_path = entity.source_file_path.as_ref().map(|path| {
            Path::new(path)
                .strip_prefix(project_root)
                .ok()
                .and_then(|rel| rel.to_str())
                .unwrap_or(path)
                .to_owned()
        });

        let mut tags = HashSet::new();
        let mut stmt = conn.prepare("SELECT tag FROM entity_tags WHERE entity_id = ?1")?;
        let mut rows = stmt.query(params![entity.id])?;
        while let Some(row) = rows.next()? {
            tags.insert(row.get::<_, String>(0)?);
        }

        let subsystem_id = subsystem_of_entity(conn, &entity.id)?.map(|found| found.subsystem_id);

        Ok(Self {
            kind: entity.kind.clone(),
            rel_path,
            tags,
            subsystem_id,
            entity_id: entity.id.clone(),
        })
    }

    /// Resolve an entity by id, then build its facts. Returns `None` when the id
    /// is unknown.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Sqlite`] on any `SQLite` failure.
    pub fn from_entity_id(
        conn: &Connection,
        entity_id: &str,
        project_root: &Path,
    ) -> Result<Option<Self>> {
        let Some(entity) = entity_by_id(conn, entity_id)? else {
            return Ok(None);
        };
        Ok(Some(Self::from_entity_row(conn, &entity, project_root)?))
    }
}

/// The verdict of evaluating one guidance `match_rule` against an entity's
/// [`MatchFacts`]. The `Matched(&'static str)` label is load-bearing: the MCP
/// `guidance_for` read path surfaces it as the sheet's `matched_by` reason, and
/// `Unevaluable` drives its `wardline_group` skip signal. Do not rename the
/// labels.
pub enum RuleVerdict {
    /// The rule matched; the static label is the rule-type name (`"path"`,
    /// `"tag"`, `"kind"`, `"subsystem"`, `"entity"`).
    Matched(&'static str),
    /// The rule did not match (or was malformed).
    NoMatch,
    /// The rule cannot be evaluated against static facts (`wardline_group`,
    /// which would require parsing the opaque Wardline blob).
    Unevaluable,
}

/// Evaluate one guidance `match_rule` (a `{"type": …, …}` object) against an
/// entity's [`MatchFacts`]. The single shared dispatch behind both
/// `guidance_sheet_matches_entity` (CLI) and `tool_guidance_for` (MCP), so the
/// two surfaces cannot drift on rule semantics.
#[must_use]
pub fn rule_match(rule: &Value, facts: &MatchFacts) -> RuleVerdict {
    let Some(rule_type) = rule.get("type").and_then(Value::as_str) else {
        return RuleVerdict::NoMatch;
    };
    match rule_type {
        "path" => match (
            rule.get("pattern").and_then(Value::as_str),
            facts.rel_path.as_deref(),
        ) {
            (Some(pattern), Some(path)) if glob_match(pattern, path) => {
                RuleVerdict::Matched("path")
            }
            _ => RuleVerdict::NoMatch,
        },
        "tag" => match rule.get("value").and_then(Value::as_str) {
            Some(value) if facts.tags.contains(value) => RuleVerdict::Matched("tag"),
            _ => RuleVerdict::NoMatch,
        },
        "kind" => match rule.get("value").and_then(Value::as_str) {
            Some(value) if value == facts.kind => RuleVerdict::Matched("kind"),
            _ => RuleVerdict::NoMatch,
        },
        "subsystem" => match (
            rule.get("id").and_then(Value::as_str),
            facts.subsystem_id.as_deref(),
        ) {
            (Some(id), Some(sub)) if id == sub => RuleVerdict::Matched("subsystem"),
            _ => RuleVerdict::NoMatch,
        },
        "entity" => match rule.get("id").and_then(Value::as_str) {
            Some(id) if id == facts.entity_id => RuleVerdict::Matched("entity"),
            _ => RuleVerdict::NoMatch,
        },
        "wardline_group" => RuleVerdict::Unevaluable,
        _ => RuleVerdict::NoMatch,
    }
}
