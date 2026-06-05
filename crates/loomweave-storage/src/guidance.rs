//! Guidance-sheet write API (WS6 / REQ-GUIDANCE-01, REQ-GUIDANCE-03).
//!
//! Guidance sheets are entities with `kind = 'guidance'` and id
//! `core:guidance:<slug>`. They are operator-authored, have **no source file**,
//! and exist **outside any `loomweave analyze` run** — so they must NOT go through
//! the run-scoped writer actor (`WriterCmd::InsertEntity`), which hard-requires
//! a `BeginRun` and a source-file anchor. Instead they insert via a plain,
//! non-run-scoped `INSERT INTO entities`, exactly the shape proven by the
//! storage schema test `entity_generated_columns_extract_from_properties_json`.
//!
//! The `properties` JSON this module writes is the contract the read path
//! (`loomweave-mcp` `catalogue::inspection::tool_guidance_for` / `rule_match`)
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
use serde::{Deserialize, Serialize};
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

    /// `$.reviewed_at` from properties. Optional and not currently populated by
    /// any write path, but honoured if present (an operator or a future
    /// "mark reviewed" verb may set it).
    fn reviewed_at(&self) -> Option<&str> {
        self.properties.get("reviewed_at").and_then(Value::as_str)
    }

    /// The instant this sheet was last "touched" for review-cadence purposes:
    /// the later of `reviewed_at` and `authored_at` (lexical max — both are the
    /// same fixed-width `YYYY-MM-DDTHH:MM:SS.mmmZ` shape, so byte order is
    /// instant order). Returns `None` when neither is present.
    fn touched_at(&self) -> Option<&str> {
        match (self.reviewed_at(), self.authored_at()) {
            (Some(r), Some(a)) => Some(r.max(a)),
            (Some(r), None) => Some(r),
            (None, Some(a)) => Some(a),
            (None, None) => None,
        }
    }
}

/// True if `sheet`'s `expires` instant is in the past relative to `now`.
///
/// This is the **review-cadence/expiry** predicate that mirrors the MCP
/// `guidance_for` read path's expiry exclusion and the
/// `LMWV-FACT-GUIDANCE-EXPIRED` finding: parse both values to Unix seconds,
/// accepting either `unix:<seconds>` or RFC3339 timestamps, and compare
/// numerically. Fail open: a sheet with no `expires`, an unparseable `expires`,
/// or an unparseable clock is never hidden as expired.
#[must_use]
pub fn guidance_sheet_is_expired(sheet: &GuidanceSheet, now: &str) -> bool {
    sheet
        .properties
        .get("expires")
        .and_then(Value::as_str)
        .and_then(parse_guidance_timestamp_to_unix_seconds)
        .zip(parse_guidance_timestamp_to_unix_seconds(now))
        .is_some_and(|(expires, now)| expires < now)
}

fn parse_guidance_timestamp_to_unix_seconds(value: &str) -> Option<i64> {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;

    if let Some(rest) = value.strip_prefix("unix:") {
        return rest.trim().parse().ok();
    }
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .map(OffsetDateTime::unix_timestamp)
}

/// True if `sheet` has not been "touched" since `stale_before` — the
/// **age/review-cadence** staleness of system-design.md §7 line 741
/// ("sheets not touched in N days"). "Touched" is the later of `reviewed_at`
/// and `authored_at`; the sheet is stale when that instant is strictly older
/// than `stale_before` (the caller's `now − N days` cutoff, in the same
/// fixed-width ISO-8601 shape so the compare is lexical). A sheet with neither
/// timestamp has no measurable age and is treated as **not stale**.
///
/// NOTE: this is age-based staleness, distinct from the churn-based signal the
/// `LMWV-FACT-GUIDANCE-CHURN-STALE` finding surfaces (which aggregates git churn
/// over matched entities). Do not conflate the two.
#[must_use]
pub fn guidance_sheet_is_stale(sheet: &GuidanceSheet, stale_before: &str) -> bool {
    sheet
        .touched_at()
        .is_some_and(|touched| touched < stale_before)
}

const SELECT_COLUMNS: &str = "id, name, short_name, scope_level, properties, \
     scope_rank, created_at, updated_at";

/// The reserved id prefix every guidance sheet's id must carry: `plugin_id`
/// `core`, reserved kind `guidance` (ADR-003 + ADR-022). The third segment (the
/// canonical name) follows.
const GUIDANCE_ID_PREFIX: &str = "core:guidance:";

/// Marker wrapping a Loomweave guidance proposal inside a Filigree observation's
/// free-form detail field. The marker lets promotion parse only observations
/// that deliberately carry the guidance payload, rather than treating arbitrary
/// scratchpad prose as trusted sheet data.
pub const GUIDANCE_PROPOSAL_MARKER: &str = "BEGIN_LOOMWEAVE_GUIDANCE_PROPOSAL_V1";
const GUIDANCE_PROPOSAL_END_MARKER: &str = "END_LOOMWEAVE_GUIDANCE_PROPOSAL_V1";

const PROVENANCE_FILIGREE_PROMOTION: &str = "filigree_promotion";
const GUIDANCE_SCOPE_LEVELS: &[&str] = &[
    "project",
    "subsystem",
    "package",
    "module",
    "class",
    "function",
];

/// The reviewed payload an MCP `propose_guidance` call stores in a Filigree
/// observation. A proposal is inert: until [`GuidanceProposal::to_promoted_sheet`]
/// is called by an operator-controlled promotion path and the resulting sheet is
/// written, it is not a `kind='guidance'` entity and cannot enter prompts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuidanceProposal {
    pub entity_id: String,
    pub content: String,
    pub scope_level: String,
    pub match_rules: Vec<Value>,
    pub name: Option<String>,
    pub pinned: bool,
    pub expires: Option<String>,
}

/// Fully-owned guidance sheet data produced from a promoted observation.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotedGuidanceSheet {
    pub id: String,
    pub name: String,
    pub short_name: String,
    pub properties: Value,
}

impl GuidanceProposal {
    /// Build the default proposal shape for an entity-targeted suggestion.
    #[must_use]
    pub fn for_entity(entity_id: &str, content: &str) -> Self {
        Self {
            entity_id: entity_id.to_owned(),
            content: content.to_owned(),
            scope_level: "function".to_owned(),
            match_rules: vec![json!({ "type": "entity", "id": entity_id })],
            name: None,
            pinned: false,
            expires: None,
        }
    }

    /// Serialize the proposal into the observation detail envelope.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidQuery`] when JSON serialization fails.
    pub fn to_observation_detail(&self) -> Result<String> {
        self.validate()?;
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| StorageError::InvalidQuery(format!("serialize guidance proposal: {e}")))?;
        Ok(format!(
            "Loomweave guidance proposal. Promote with `loomweave guidance promote` after review.\n\n\
             {GUIDANCE_PROPOSAL_MARKER}\n{json}\n{GUIDANCE_PROPOSAL_END_MARKER}\n"
        ))
    }

    /// Parse a guidance proposal from a Filigree observation detail string.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidQuery`] when the marker is missing, JSON is
    /// malformed, or the decoded proposal violates sheet invariants.
    pub fn from_observation_detail(detail: &str) -> Result<Self> {
        let start = detail.find(GUIDANCE_PROPOSAL_MARKER).ok_or_else(|| {
            StorageError::InvalidQuery(
                "observation does not contain a Loomweave guidance proposal".to_owned(),
            )
        })? + GUIDANCE_PROPOSAL_MARKER.len();
        let rest = &detail[start..];
        let end = rest.find(GUIDANCE_PROPOSAL_END_MARKER).ok_or_else(|| {
            StorageError::InvalidQuery(
                "Loomweave guidance proposal is missing its end marker".to_owned(),
            )
        })?;
        let raw_json = rest[..end].trim();
        let proposal: Self = serde_json::from_str(raw_json).map_err(|e| {
            StorageError::InvalidQuery(format!("parse Loomweave guidance proposal: {e}"))
        })?;
        proposal.validate()?;
        Ok(proposal)
    }

    /// Convert this reviewed proposal into a guidance sheet payload.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidQuery`] if the proposal is malformed.
    pub fn to_promoted_sheet(&self, authored_at: &str) -> Result<PromotedGuidanceSheet> {
        self.validate()?;
        let slug_source = self.name.as_deref().unwrap_or(&self.entity_id);
        let name = slugify_guidance_name(slug_source);
        let short_name = name.rsplit('.').next().unwrap_or(&name).to_owned();
        let id = format!("{GUIDANCE_ID_PREFIX}{name}");
        let mut properties = json!({
            "content": self.content,
            "scope_level": self.scope_level,
            "match_rules": self.match_rules,
            "pinned": self.pinned,
            "provenance": PROVENANCE_FILIGREE_PROMOTION,
            "authored_at": authored_at,
            "proposed_for_entity": self.entity_id,
        });
        if let Some(expires) = &self.expires
            && let Some(obj) = properties.as_object_mut()
        {
            obj.insert("expires".to_owned(), json!(expires));
        }
        Ok(PromotedGuidanceSheet {
            id,
            name,
            short_name,
            properties,
        })
    }

    fn validate(&self) -> Result<()> {
        if self.entity_id.trim().is_empty() {
            return Err(StorageError::InvalidQuery(
                "guidance proposal missing entity_id".to_owned(),
            ));
        }
        if self.content.trim().is_empty() {
            return Err(StorageError::InvalidQuery(
                "guidance proposal content is empty".to_owned(),
            ));
        }
        if !GUIDANCE_SCOPE_LEVELS.contains(&self.scope_level.as_str()) {
            return Err(StorageError::InvalidQuery(format!(
                "guidance proposal scope_level '{}' is invalid",
                self.scope_level
            )));
        }
        if self.match_rules.is_empty() {
            return Err(StorageError::InvalidQuery(
                "guidance proposal needs at least one match rule".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Derive a canonical guidance-name slug. Kept here so CLI, MCP, and Wardline-
/// derived generation all mint ids with the same grammar.
#[must_use]
pub fn slugify_guidance_name(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        "guidance".to_owned()
    } else {
        trimmed
    }
}

fn validate_guidance_id(id: &str) -> Result<()> {
    let Some(name) = id.strip_prefix(GUIDANCE_ID_PREFIX) else {
        return Err(StorageError::InvalidQuery(format!(
            "guidance sheet id '{id}' is not a guidance id (must start with `{GUIDANCE_ID_PREFIX}`); \
             refusing to write — this would corrupt the entity it names"
        )));
    };
    if name.is_empty() {
        return Err(StorageError::InvalidQuery(format!(
            "guidance sheet id '{id}' is missing the guidance name after `{GUIDANCE_ID_PREFIX}`"
        )));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(StorageError::InvalidQuery(format!(
            "guidance sheet id '{id}' contains invalid characters; guidance names may only contain \
             ASCII letters, numbers, '.', '-', and '_'"
        )));
    }
    Ok(())
}

/// Insert a new guidance sheet. Unlike [`upsert_guidance_sheet`], this is
/// create-only: an existing id is reported as an error and the stored row is
/// left unchanged.
///
/// # Errors
///
/// Returns [`StorageError::InvalidQuery`] if `sheet.id` does not start with
/// `core:guidance:` or if a row with the same id already exists. Returns
/// [`StorageError::Sqlite`] on any other `SQLite` failure.
pub fn insert_guidance_sheet(conn: &Connection, sheet: &GuidanceSheetInput<'_>) -> Result<()> {
    validate_guidance_id(sheet.id)?;
    let properties = serde_json::to_string(sheet.properties)
        .map_err(|e| StorageError::InvalidQuery(format!("serialize guidance properties: {e}")))?;
    let rows = conn.execute(
        "INSERT INTO entities \
            (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
         VALUES \
            (?1, 'core', 'guidance', ?2, ?3, ?4, \
             strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now')) \
         ON CONFLICT(id) DO NOTHING",
        params![sheet.id, sheet.name, sheet.short_name, properties],
    )?;
    if rows == 0 {
        return Err(StorageError::InvalidQuery(format!(
            "guidance sheet '{}' already exists; use edit to modify it",
            sheet.id
        )));
    }
    Ok(())
}

/// Insert or replace a guidance sheet. On a fresh id this inserts; on an
/// existing id it updates `name`, `short_name`, `properties`, and bumps
/// `updated_at` (preserving `created_at`). The generated columns recompute from
/// the new `properties` automatically.
///
/// This is the low-level overwrite primitive. The CLI's `create` guards against
/// clobbering an existing id (that is `edit`'s job); `edit` does a
/// read-modify-write that preserves `authored_at` / `provenance` / `pinned`.
///
/// **Id guard (graph-integrity invariant):** the id MUST carry the
/// `core:guidance:` prefix. This protects ALL write paths
/// (create / edit / import) from a hand-edited or malicious payload whose id
/// names a *code* entity (e.g. `python:function:foo`): without the guard the
/// `ON CONFLICT(id) DO UPDATE` would overwrite that code entity's
/// `name`/`properties` (leaving its `kind`/`plugin_id`), silently corrupting the
/// entity graph. The `ON CONFLICT` clause is additionally scoped
/// `WHERE kind = 'guidance'` as defense-in-depth, but the prefix check is the
/// primary gate.
///
/// # Errors
///
/// Returns [`StorageError::InvalidQuery`] if `sheet.id` does not start with
/// `core:guidance:` (nothing is written). Returns [`StorageError::Sqlite`] on
/// any `SQLite` failure (lock, constraint).
pub fn upsert_guidance_sheet(conn: &Connection, sheet: &GuidanceSheetInput<'_>) -> Result<()> {
    validate_guidance_id(sheet.id)?;
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
            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') \
         WHERE kind = 'guidance'",
        params![sheet.id, sheet.name, sheet.short_name, properties],
    )?;
    Ok(())
}

/// Upsert a [`PortableSheet`] (the import primitive, WS6 / T5).
///
/// Additive by design: it `upsert`s the one sheet, re-deriving `short_name` from
/// `name`, and leaves every other sheet in the DB untouched. Import is therefore
/// a **merge**, never a mirror — it never deletes a local sheet absent from the
/// imported set (a mirror would be silent destruction of local knowledge). Re-
/// importing identical bytes is a no-op on content (only `updated_at` moves).
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] on any `SQLite` failure.
pub fn import_portable_sheet(conn: &Connection, sheet: &PortableSheet) -> Result<()> {
    upsert_guidance_sheet(
        conn,
        &GuidanceSheetInput {
            id: &sheet.id,
            name: &sheet.name,
            short_name: sheet.short_name(),
            properties: &sheet.properties,
        },
    )
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

// ── Portable (export/import) form (WS6 / T5, REQ-GUIDANCE-06) ──────────────────

/// The git-shareable, diff-friendly form of one guidance sheet.
///
/// A team commits these files to a repo to share institutional knowledge, so the
/// serialization is engineered for **determinism** (identical DB state → byte-
/// identical bytes) and **diff-friendliness** (a one-field change is a one-line
/// diff). It carries only the sheet's **portable** content:
///   - `id`   — the full entity id (`core:guidance:<name>`); preserved exactly.
///   - `name` — `entities.name` (segment 3 of the id).
///   - `properties` — the verbatim `entities.properties` object (`content`,
///     `scope_level`, `match_rules`, `pinned`, `provenance`, `authored_at`, …).
///
/// Deliberately **omitted**: `created_at` / `updated_at`. Those are per-DB write
/// bookkeeping — they differ across machines and re-import, so exporting them
/// would inject spurious, non-deterministic diffs. `short_name` is also omitted:
/// it is re-derived on import from `name` exactly as the authoring path does, so
/// it can never drift from `create`'s convention.
///
/// Determinism rests on `serde_json::Map` being a `BTreeMap` in this build (no
/// `preserve_order` feature), so [`Self::to_canonical_json`] emits map keys in
/// sorted order recursively; arrays (e.g. `match_rules`) keep author order, which
/// is the intended semantic. See [`Self::to_canonical_json`] for the byte contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortableSheet {
    /// Full entity id (`core:guidance:<name>`).
    pub id: String,
    /// `entities.name` — the canonical qualified name.
    pub name: String,
    /// The verbatim `entities.properties` object.
    pub properties: Value,
}

impl PortableSheet {
    /// Project a stored [`GuidanceSheet`] down to its portable form, dropping the
    /// per-DB `created_at` / `updated_at` bookkeeping and the re-derivable
    /// `short_name`.
    #[must_use]
    pub fn from_sheet(sheet: &GuidanceSheet) -> Self {
        Self {
            id: sheet.id.clone(),
            name: sheet.name.clone(),
            properties: sheet.properties.clone(),
        }
    }

    /// The `short_name` to store on import: the display tail of `name`, derived
    /// exactly as the CLI `create` path does (`name.rsplit('.').next()`), so an
    /// imported sheet is byte-indistinguishable from a locally-authored one.
    #[must_use]
    pub fn short_name(&self) -> &str {
        self.name.rsplit('.').next().unwrap_or(&self.name)
    }

    /// Serialize to canonical, diff-friendly JSON **with a trailing newline**.
    ///
    /// "Canonical" = pretty-printed (one field per line, so a single changed
    /// field is a single changed line) with **sorted** object keys at every
    /// depth. Key order is sorted because `serde_json::Map` is a `BTreeMap` in
    /// this build; `to_string_pretty` walks it in `BTreeMap` (sorted) order. The
    /// struct's own three keys (`id`, `name`, `properties`) are likewise emitted
    /// sorted — `id` < `name` < `properties` alphabetically, a stable order. The
    /// trailing `\n` is POSIX-text hygiene and keeps git from flagging a
    /// "no newline at end of file".
    ///
    /// This is the **only** place output bytes are formed; nothing on the export
    /// path uses `HashMap` iteration order or embeds an export timestamp / path.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidQuery`] if serialization fails (it cannot,
    /// for a `Value`-backed struct, but the fallible signature avoids a panic).
    pub fn to_canonical_json(&self) -> Result<String> {
        let mut out = serde_json::to_string_pretty(self)
            .map_err(|e| StorageError::InvalidQuery(format!("serialize guidance sheet: {e}")))?;
        out.push('\n');
        Ok(out)
    }

    /// Parse a [`PortableSheet`] from the canonical JSON bytes (`source` names the
    /// file, for a loud error). Rejects an empty `id` or `name` — a sheet without
    /// either cannot be upserted and signals a corrupt/hand-mangled file.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidQuery`] naming `source` on malformed JSON or
    /// a missing/empty `id` / `name`. Import callers surface this as a hard
    /// failure (a dropped sheet is silent data loss).
    pub fn from_canonical_json(source: &str, bytes: &str) -> Result<Self> {
        let sheet: Self = serde_json::from_str(bytes).map_err(|e| {
            StorageError::InvalidQuery(format!("parse guidance sheet {source}: {e}"))
        })?;
        if sheet.id.trim().is_empty() {
            return Err(StorageError::InvalidQuery(format!(
                "guidance sheet {source}: missing or empty `id`"
            )));
        }
        if sheet.name.trim().is_empty() {
            return Err(StorageError::InvalidQuery(format!(
                "guidance sheet {source}: missing or empty `name`"
            )));
        }
        validate_guidance_id(&sheet.id)
            .map_err(|e| StorageError::InvalidQuery(format!("guidance sheet {source}: {e}")))?;
        Ok(sheet)
    }

    /// The deterministic, filesystem-safe filename for this sheet.
    ///
    /// The entity id contains colons (`core:guidance:foo.bar`), which are not
    /// portable across filesystems (illegal on Windows/NTFS, awkward in shells).
    /// We map each `:` to `__` (double underscore) and append `.json`, giving
    /// e.g. `core__guidance__foo.bar.json`. The mapping is **deterministic** and
    /// **collision-free**: `:` is a reserved id separator (ADR-003 entity ids are
    /// exactly three colon-joined segments and the segments never contain a bare
    /// colon by construction), so distinct ids never collide after substitution.
    /// We do not need to reverse the filename — the authoritative id lives inside
    /// the file — so the encoding only has to be injective for valid guidance ids,
    /// not invertible. Any unexpected legacy/hand-written separator byte is
    /// flattened defensively so export can never traverse out of the target
    /// directory.
    #[must_use]
    pub fn file_name(&self) -> String {
        let mut stem = String::with_capacity(self.id.len());
        for byte in self.id.bytes() {
            match byte {
                b':' => stem.push_str("__"),
                b if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') => {
                    stem.push(char::from(b));
                }
                _ => stem.push('_'),
            }
        }
        format!("{stem}.json")
    }
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
/// blob is opaque to Loomweave).
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
/// (`loomweave guidance create|edit|delete`) calls this on every mutation.
///
/// Scan strategy: drive off `SELECT DISTINCT entity_id FROM summary_cache` (the
/// only entities that *can* be invalidated), not the whole entity table — this
/// keeps the work O(cached-entities) ≤ O(N-entities) and, by reusing
/// [`crate::cache::delete_summary_cache_for_entity`]'s single-entity `DELETE`, dodges the
/// `SQLite` 999-bound-parameter ceiling a broad `IN (…)` over a wide `path:`
/// match would otherwise hit on a large corpus. Guidance sheets never carry
/// cache rows, so the `kind = 'guidance'` exclusion is automatic.
///
/// A sheet applies to an entity if EITHER a `match_rules` rule fires OR the sheet
/// has an explicit `guides` edge to it — the same OR composition the MCP
/// `guidance_for` read path uses. This function honours both: it collects the
/// sheet's `guides`-edge targets (`SELECT to_id FROM edges WHERE kind = 'guides'
/// AND from_id = ?sheet_id`) and invalidates them alongside the rule matches.
/// An entity reached by both a rule and a guides edge is invalidated exactly
/// once (the `cached_ids`-driven loop de-dups automatically). A sheet with no
/// `match_rules` and no `guides` edges matches nothing and this is a clean 0-row
/// no-op.
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

    // The sheet's explicit `guides`-edge targets. `guidance_for` composes these
    // OR-wise with `match_rules`, so invalidation must too. Driving the delete off
    // `cached_ids` (below) with an OR'd predicate keeps the count exact and
    // de-dups an entity reached by both a rule and a guides edge automatically.
    let guides_targets: HashSet<String> = {
        let mut stmt =
            conn.prepare("SELECT to_id FROM edges WHERE kind = 'guides' AND from_id = ?1")?;
        let rows = stmt.query_map(params![sheet.id], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let mut removed = 0usize;
    for entity_id in &cached_ids {
        if guides_targets.contains(entity_id)
            || guidance_sheet_matches_entity(conn, sheet, entity_id, &canonical_root)?
        {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a bare `GuidanceSheet` carrying only the given properties object,
    /// for testing the pure date predicates (no DB needed).
    fn sheet_with(properties: Value) -> GuidanceSheet {
        GuidanceSheet {
            id: "core:guidance:test".to_owned(),
            name: "test".to_owned(),
            short_name: "test".to_owned(),
            scope_level: Some("module".to_owned()),
            scope_rank: Some(4),
            properties,
            created_at: "2026-01-01T00:00:00.000Z".to_owned(),
            updated_at: "2026-01-01T00:00:00.000Z".to_owned(),
        }
    }

    #[test]
    fn guidance_proposal_detail_round_trips_to_promoted_sheet() {
        let proposal = GuidanceProposal {
            entity_id: "python:function:demo.entry".to_owned(),
            content: "Prefer operational risk notes.".to_owned(),
            scope_level: "function".to_owned(),
            match_rules: vec![json!({"type": "entity", "id": "python:function:demo.entry"})],
            name: Some("demo-entry-risk".to_owned()),
            pinned: true,
            expires: Some("2026-12-31T00:00:00.000Z".to_owned()),
        };

        let detail = proposal
            .to_observation_detail()
            .expect("serialize proposal detail");
        assert!(detail.contains(GUIDANCE_PROPOSAL_MARKER));

        let parsed = GuidanceProposal::from_observation_detail(&detail)
            .expect("parse proposal from observation detail");
        assert_eq!(parsed, proposal);

        let sheet = parsed
            .to_promoted_sheet("2026-06-04T00:00:00.000Z")
            .expect("build promoted sheet");
        assert_eq!(sheet.id, "core:guidance:demo-entry-risk");
        assert_eq!(sheet.name, "demo-entry-risk");
        assert_eq!(sheet.short_name, "demo-entry-risk");
        assert_eq!(
            sheet.properties.get("provenance").and_then(Value::as_str),
            Some("filigree_promotion")
        );
        assert_eq!(
            sheet.properties.get("authored_at").and_then(Value::as_str),
            Some("2026-06-04T00:00:00.000Z")
        );
        assert_eq!(
            sheet
                .properties
                .get("match_rules")
                .and_then(Value::as_array)
                .and_then(|rules| rules.first())
                .and_then(|rule| rule.get("id"))
                .and_then(Value::as_str),
            Some("python:function:demo.entry")
        );
    }

    // ── guidance_sheet_is_expired ────────────────────────────────────────────

    #[test]
    fn expired_past_expires_is_expired() {
        let sheet = sheet_with(json!({ "expires": "2026-01-01T00:00:00.000Z" }));
        assert!(guidance_sheet_is_expired(
            &sheet,
            "2026-06-03T12:00:00.000Z"
        ));
    }

    #[test]
    fn expired_future_expires_is_not_expired() {
        let sheet = sheet_with(json!({ "expires": "2999-01-01T00:00:00.000Z" }));
        assert!(!guidance_sheet_is_expired(
            &sheet,
            "2026-06-03T12:00:00.000Z"
        ));
    }

    #[test]
    fn expired_absent_expires_is_not_expired() {
        let sheet = sheet_with(json!({ "authored_at": "2026-01-01T00:00:00.000Z" }));
        assert!(!guidance_sheet_is_expired(
            &sheet,
            "2026-06-03T12:00:00.000Z"
        ));
    }

    #[test]
    fn expired_equal_expires_is_not_expired() {
        // `expires < now` is strict: a sheet expiring exactly at `now` is not
        // yet expired (mirrors the read path's `<` compare).
        let sheet = sheet_with(json!({ "expires": "2026-06-03T12:00:00.000Z" }));
        assert!(!guidance_sheet_is_expired(
            &sheet,
            "2026-06-03T12:00:00.000Z"
        ));
    }

    #[test]
    fn expired_future_expires_is_not_expired_with_unix_clock() {
        let sheet = sheet_with(json!({ "expires": "2999-01-01T00:00:00.000Z" }));
        assert!(!guidance_sheet_is_expired(&sheet, "unix:1748822400"));
    }

    #[test]
    fn expired_past_expires_is_expired_with_unix_clock() {
        let sheet = sheet_with(json!({ "expires": "2000-01-01T00:00:00.000Z" }));
        assert!(guidance_sheet_is_expired(&sheet, "unix:1748822400"));
    }

    #[test]
    fn expired_unparseable_clock_fails_open() {
        let sheet = sheet_with(json!({ "expires": "2000-01-01T00:00:00.000Z" }));
        assert!(!guidance_sheet_is_expired(&sheet, "not-a-clock"));
    }

    // ── guidance_sheet_is_stale ──────────────────────────────────────────────

    #[test]
    fn stale_old_authored_is_stale() {
        // authored long ago, no reviewed_at → touched = authored < cutoff.
        let sheet = sheet_with(json!({ "authored_at": "2026-01-01T00:00:00.000Z" }));
        let cutoff = "2026-03-05T12:00:00.000Z"; // now − 90 days, roughly
        assert!(guidance_sheet_is_stale(&sheet, cutoff));
    }

    #[test]
    fn stale_fresh_authored_is_not_stale() {
        let sheet = sheet_with(json!({ "authored_at": "2026-06-01T00:00:00.000Z" }));
        let cutoff = "2026-03-05T12:00:00.000Z";
        assert!(!guidance_sheet_is_stale(&sheet, cutoff));
    }

    #[test]
    fn stale_recent_reviewed_at_overrides_old_authored_at() {
        // Old authored_at but a recent reviewed_at → touched = max = reviewed_at,
        // which is after the cutoff, so the sheet is NOT stale. This is the named
        // TDD target: reviewed_at (when later) is what counts.
        let sheet = sheet_with(json!({
            "authored_at": "2025-01-01T00:00:00.000Z",
            "reviewed_at": "2026-06-01T00:00:00.000Z",
        }));
        let cutoff = "2026-03-05T12:00:00.000Z";
        assert!(!guidance_sheet_is_stale(&sheet, cutoff));
    }

    #[test]
    fn stale_old_reviewed_at_still_stale() {
        // Both old → touched = max is still before the cutoff → stale.
        let sheet = sheet_with(json!({
            "authored_at": "2025-01-01T00:00:00.000Z",
            "reviewed_at": "2025-02-01T00:00:00.000Z",
        }));
        let cutoff = "2026-03-05T12:00:00.000Z";
        assert!(guidance_sheet_is_stale(&sheet, cutoff));
    }

    #[test]
    fn stale_no_timestamps_is_not_stale() {
        // Neither authored_at nor reviewed_at → unmeasurable age → not stale.
        let sheet = sheet_with(json!({ "content": "x" }));
        let cutoff = "2026-03-05T12:00:00.000Z";
        assert!(!guidance_sheet_is_stale(&sheet, cutoff));
    }

    #[test]
    fn stale_equal_to_cutoff_is_not_stale() {
        // `touched < stale_before` is strict.
        let sheet = sheet_with(json!({ "authored_at": "2026-03-05T12:00:00.000Z" }));
        let cutoff = "2026-03-05T12:00:00.000Z";
        assert!(!guidance_sheet_is_stale(&sheet, cutoff));
    }

    // ── PortableSheet (export/import) ─────────────────────────────────────────

    fn portable_with(id: &str, name: &str, properties: Value) -> PortableSheet {
        PortableSheet {
            id: id.to_owned(),
            name: name.to_owned(),
            properties,
        }
    }

    #[test]
    fn canonical_json_has_trailing_newline() {
        let p = portable_with("core:guidance:x", "x", json!({ "content": "y" }));
        let json = p.to_canonical_json().unwrap();
        assert!(json.ends_with('\n'), "must end with a newline: {json:?}");
        assert!(!json.ends_with("\n\n"), "exactly one newline: {json:?}");
    }

    #[test]
    fn canonical_json_sorts_keys_for_diff_stability() {
        // Author the properties with keys in NON-sorted order; the serialized
        // bytes must come out sorted (so a re-serialize from any key order is
        // byte-stable). `serde_json::Map` is a BTreeMap in this build, so this
        // holds recursively.
        let p = portable_with(
            "core:guidance:s",
            "s",
            json!({ "zeta": 1, "alpha": 2, "nested": { "yray": 1, "beta": 2 } }),
        );
        let json = p.to_canonical_json().unwrap();
        let alpha = json.find("alpha").unwrap();
        let zeta = json.find("zeta").unwrap();
        assert!(alpha < zeta, "top-level keys sorted: {json}");
        let beta = json.find("beta").unwrap();
        let yray = json.find("yray").unwrap();
        assert!(beta < yray, "nested keys sorted: {json}");
    }

    #[test]
    fn canonical_json_is_deterministic_across_runs() {
        // Two PortableSheets built from differently-ordered property maps but the
        // same logical content must serialize byte-identically.
        let a = portable_with(
            "core:guidance:d",
            "d",
            json!({ "b": 1, "a": 2, "c": [3, 2, 1] }),
        );
        let b = portable_with(
            "core:guidance:d",
            "d",
            json!({ "c": [3, 2, 1], "a": 2, "b": 1 }),
        );
        assert_eq!(
            a.to_canonical_json().unwrap(),
            b.to_canonical_json().unwrap()
        );
    }

    #[test]
    fn canonical_json_preserves_array_order() {
        // match_rules order is semantic (first-match precedence) — arrays must NOT
        // be reordered, only object keys.
        let p = portable_with(
            "core:guidance:r",
            "r",
            json!({ "match_rules": [{ "type": "path" }, { "type": "kind" }] }),
        );
        let json = p.to_canonical_json().unwrap();
        assert!(
            json.find("path").unwrap() < json.find("kind").unwrap(),
            "array element order preserved: {json}"
        );
    }

    #[test]
    fn portable_json_round_trips() {
        let p = portable_with(
            "core:guidance:rt",
            "auth.tokens",
            json!({
                "content": "guard the refresh path",
                "scope_level": "module",
                "match_rules": [{ "type": "path", "pattern": "src/auth/**" }],
                "pinned": true,
                "provenance": "manual",
                "authored_at": "2026-01-01T00:00:00.000Z",
                "expires": "2027-01-01T00:00:00.000Z",
            }),
        );
        let json = p.to_canonical_json().unwrap();
        let back = PortableSheet::from_canonical_json("rt.json", &json).unwrap();
        assert_eq!(back.id, p.id);
        assert_eq!(back.name, p.name);
        assert_eq!(back.properties, p.properties);
    }

    #[test]
    fn file_name_sanitizes_colons() {
        let p = portable_with("core:guidance:foo.bar", "foo.bar", json!({}));
        assert_eq!(p.file_name(), "core__guidance__foo.bar.json");
    }

    #[test]
    fn file_name_flattens_legacy_path_separators() {
        let p = portable_with("core:guidance:../escaped", "../escaped", json!({}));
        assert_eq!(p.file_name(), "core__guidance__.._escaped.json");
    }

    #[test]
    fn short_name_is_display_tail() {
        let p = portable_with("core:guidance:a.b.c", "a.b.c", json!({}));
        assert_eq!(p.short_name(), "c");
        let flat = portable_with("core:guidance:flat", "flat", json!({}));
        assert_eq!(flat.short_name(), "flat");
    }

    #[test]
    fn from_canonical_json_rejects_malformed() {
        assert!(PortableSheet::from_canonical_json("bad.json", "{ not json").is_err());
        // valid JSON but not a sheet (missing id/name) → error naming the file.
        let err = PortableSheet::from_canonical_json("nmeta.json", "{\"properties\": {}}")
            .unwrap_err()
            .to_string();
        assert!(err.contains("nmeta.json"), "error names the file: {err}");
    }

    #[test]
    fn from_canonical_json_rejects_empty_id() {
        let err = PortableSheet::from_canonical_json("empty.json", "{\"id\":\"\",\"name\":\"n\"}")
            .unwrap_err()
            .to_string();
        assert!(err.contains("empty.json"), "{err}");
    }

    #[test]
    fn from_canonical_json_rejects_non_guidance_id() {
        let err = PortableSheet::from_canonical_json(
            "evil.json",
            r#"{"id":"python:function:auth.tokens.refresh","name":"auth.tokens.refresh","properties":{}}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("evil.json"), "{err}");
        assert!(err.contains("core:guidance:"), "{err}");
    }

    #[test]
    fn from_canonical_json_rejects_path_separator_in_id() {
        let err = PortableSheet::from_canonical_json(
            "traverse.json",
            r#"{"id":"core:guidance:../escaped","name":"../escaped","properties":{}}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("traverse.json"), "{err}");
        assert!(err.contains("invalid characters"), "{err}");
    }
}
