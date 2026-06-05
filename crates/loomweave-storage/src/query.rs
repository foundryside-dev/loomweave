//! Read-side query helpers used by the MCP navigation surface.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use loomweave_core::EdgeConfidence;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::{Serialize, Serializer};

use crate::{Result, StorageError};

/// A path that is *proven* to be:
///
/// 1. anchored under the project root (no `..` / `/` / drive prefix),
/// 2. composed solely of normal UTF-8 path components, and
/// 3. emitted in POSIX-style (`/`-joined) form.
///
/// The inner string is private and `try_new` is the only public constructor,
/// so a `CanonicalProjectPath` cannot exist without that proof. Serializes
/// transparently as its inner string so federation wire formats are
/// unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalProjectPath(String);

impl CanonicalProjectPath {
    /// Construct from a `normalized` absolute path under `project_root`.
    /// `normalized` is expected to already be lexically + filesystem
    /// canonicalised by the caller (see [`normalize_source_path`]); this
    /// constructor proves the residual project-relative-POSIX shape.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::InvalidSourcePath`] when the path escapes
    /// `project_root`, contains any non-`Normal` component, or is not
    /// valid UTF-8.
    pub fn try_new(project_root: &Path, normalized: &Path) -> Result<Self> {
        Ok(Self(project_relative_path(project_root, normalized)?))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for CanonicalProjectPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for CanonicalProjectPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl AsRef<str> for CanonicalProjectPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityRow {
    pub id: String,
    pub plugin_id: String,
    pub kind: String,
    pub name: String,
    pub short_name: String,
    pub parent_id: Option<String>,
    pub source_file_id: Option<String>,
    pub source_file_path: Option<String>,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
    pub source_line_start: Option<i64>,
    pub source_line_end: Option<i64>,
    pub properties_json: String,
    pub content_hash: Option<String>,
    pub summary_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEdgeMatch {
    pub from_id: String,
    pub to_id: String,
    pub stored_to_id: String,
    pub confidence: EdgeConfidence,
    pub source_file_id: Option<String>,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
    pub properties_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainedEntities {
    pub entity_ids: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedCallSiteRow {
    pub caller_entity_id: String,
    pub caller_content_hash: String,
    pub site_key: String,
    pub site_ordinal: i64,
    pub source_file_id: Option<String>,
    pub source_byte_start: i64,
    pub source_byte_end: i64,
    pub callee_expr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceEdgeMatch {
    pub neighbor_id: String,
    pub confidence: EdgeConfidence,
    pub source_file_id: Option<String>,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
}

/// One rolled-up reference edge for a module-altitude query: a `references`
/// edge into or out of a symbol the module contains, attributed to the
/// contained `via` symbol it actually touches (clarion-79d0ff6e14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RolledUpReferenceEdge {
    /// The entity on the far side of the edge — the referencer for `In`
    /// (who imports a contained symbol), the referenced symbol for `Out`.
    pub neighbor_id: String,
    /// The module-contained symbol whose edge this is. For a module's own
    /// direct reference edge (rare) this equals the module id.
    pub via_id: String,
    pub confidence: EdgeConfidence,
    pub source_file_id: Option<String>,
    pub source_byte_start: Option<i64>,
    pub source_byte_end: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleDependencyEdge {
    pub from_module_id: String,
    pub to_module_id: String,
    pub reference_count: u64,
    pub edge_kinds: Vec<String>,
}

/// One persisted finding joined to its anchoring entity's source location, in
/// the shape the cross-product emitter (`loomweave-mcp` scan-results POST,
/// WP9-B) needs: the Loomweave-internal severity/kind vocabulary plus the
/// entity's `source_file_path` / `source_line_*` that become Filigree's wire
/// `path` / `line_start` / `line_end`. `source_file_path` is `None` for
/// findings anchored to entities with no source location (e.g. a
/// `core:subsystem:*` anchor); the emitter skips those because Filigree
/// requires `path`.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingForEmitRow {
    pub id: String,
    pub rule_id: String,
    pub kind: String,
    /// Loomweave-internal severity: `INFO` | `WARN` | `ERROR` | `CRITICAL` |
    /// `NONE`. Mapped to Filigree's wire vocabulary by the emitter.
    pub severity: String,
    pub confidence: Option<f64>,
    pub confidence_basis: Option<String>,
    pub message: String,
    pub entity_id: String,
    /// JSON array text as stored in `findings.related_entities`.
    pub related_entities_json: String,
    /// JSON array text as stored in `findings.supports`.
    pub supports_json: String,
    /// JSON array text as stored in `findings.supported_by`.
    pub supported_by_json: String,
    pub source_file_path: Option<String>,
    pub source_line_start: Option<i64>,
    pub source_line_end: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFile {
    pub entity_id: String,
    pub content_hash: String,
    pub canonical_path: CanonicalProjectPath,
    pub language: String,
    /// `Some(reason)` when the resolved entity carries a `briefing_blocked`
    /// property (set by the pre-ingest secret scanner or the unscanned-source
    /// defense-in-depth path). Federation read surfaces must refuse to expose
    /// blocked entities to siblings; see `http_read::get_file` for the 404
    /// translation.
    pub briefing_blocked: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFileCatalogEntry {
    pub entity_id: String,
    pub content_hash: Option<String>,
    pub canonical_path: CanonicalProjectPath,
    pub language: String,
    pub briefing_blocked: Option<String>,
    content_hash_path: PathBuf,
}

impl ResolvedFileCatalogEntry {
    pub fn into_resolved_file(self) -> Result<ResolvedFile> {
        let content_hash = match self.content_hash {
            Some(content_hash) => content_hash,
            None => file_content_hash(&self.content_hash_path)?,
        };
        Ok(ResolvedFile {
            entity_id: self.entity_id,
            content_hash,
            canonical_path: self.canonical_path,
            language: self.language,
            briefing_blocked: self.briefing_blocked,
        })
    }
}

const MODULE_ANCESTOR_MAX_DEPTH: i64 = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsystemMember {
    pub id: String,
    pub name: String,
    pub source_file_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceDirection {
    In,
    Out,
}

#[derive(Debug, Clone)]
struct StoredCallEdge {
    from_id: String,
    stored_to_id: String,
    confidence: EdgeConfidence,
    source_file_id: Option<String>,
    source_byte_start: Option<i64>,
    source_byte_end: Option<i64>,
    properties_json: Option<String>,
}

struct StoredDependencyEdge {
    from_id: String,
    to_id: String,
    kind: String,
    confidence: EdgeConfidence,
    properties_json: Option<String>,
}

const ENTITY_COLUMNS: &str = "\
    id, plugin_id, kind, name, short_name, parent_id, source_file_id, \
    source_file_path, source_byte_start, source_byte_end, source_line_start, \
    source_line_end, properties, content_hash, summary";

pub fn normalize_source_path(project_root: &Path, file: &str) -> Result<String> {
    let root = project_root.canonicalize()?;
    let input = Path::new(file);
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    };
    let lexical = normalize_lexically(&candidate);
    if !lexical.starts_with(&root) {
        return Err(StorageError::InvalidSourcePath(format!(
            "{file:?} escapes project root {}",
            root.display()
        )));
    }
    let canonical = lexical.canonicalize()?;
    if !canonical.starts_with(&root) {
        return Err(StorageError::InvalidSourcePath(format!(
            "{file:?} escapes project root {}",
            root.display()
        )));
    }
    let Some(path) = canonical.to_str() else {
        return Err(StorageError::InvalidSourcePath(format!(
            "{file:?} is not valid UTF-8"
        )));
    };
    Ok(path.to_owned())
}

pub fn entity_by_id(conn: &Connection, entity_id: &str) -> Result<Option<EntityRow>> {
    let sql = format!("SELECT {ENTITY_COLUMNS} FROM entities WHERE id = ?1");
    conn.query_row(&sql, params![entity_id], map_entity_row)
        .optional()
        .map_err(StorageError::from)
}

pub fn resolve_file(
    conn: &Connection,
    project_root: &Path,
    file: &str,
    language: &str,
) -> Result<Option<ResolvedFile>> {
    let Some(entry) = resolve_file_catalog_entry(conn, project_root, file, language)? else {
        return Ok(None);
    };
    entry.into_resolved_file().map(Some)
}

pub fn resolve_file_catalog_entry(
    conn: &Connection,
    project_root: &Path,
    file: &str,
    language: &str,
) -> Result<Option<ResolvedFileCatalogEntry>> {
    let lookup_path = normalize_lookup_path(project_root, file)?;
    let normalized = lookup_path
        .to_str()
        .ok_or_else(|| StorageError::InvalidSourcePath(format!("{file:?} is not valid UTF-8")))?;
    let canonical_path = CanonicalProjectPath::try_new(project_root, &lookup_path)?;
    if let Some(entity) = source_entity_for_path(conn, normalized, Some("file"))? {
        let briefing_blocked = entity_briefing_block_reason(&entity.properties_json);
        return Ok(Some(ResolvedFileCatalogEntry {
            entity_id: entity.id,
            content_hash: entity.content_hash,
            canonical_path,
            language: resolved_language(
                language,
                &entity.plugin_id,
                &entity.properties_json,
                &lookup_path,
            ),
            briefing_blocked,
            content_hash_path: lookup_path,
        }));
    }
    Ok(None)
}

/// Extract the `briefing_blocked` reason from an entity's `properties` JSON
/// column. Shared with `loomweave-mcp` (which makes the same call inline) so
/// federation read surfaces enforce the block uniformly.
pub fn entity_briefing_block_reason(properties_json: &str) -> Option<String> {
    // Fail-closed: malformed properties JSON treated as briefing-blocked to prevent secret exposure.
    let Ok(value) = serde_json::from_str::<serde_json::Value>(properties_json) else {
        return Some("malformed_properties_json".to_owned());
    };
    value.get("briefing_blocked")?.as_str().map(str::to_owned)
}

fn source_entity_for_path(
    conn: &Connection,
    normalized_path: &str,
    required_kind: Option<&str>,
) -> Result<Option<EntityRow>> {
    let kind_filter = required_kind.map_or(String::new(), |_| "AND kind = ?2".to_owned());
    let sql = format!(
        "SELECT {ENTITY_COLUMNS} \
         FROM entities \
         WHERE source_file_path = ?1 \
         {kind_filter} \
         ORDER BY CASE kind \
                    WHEN 'file' THEN 0 \
                    WHEN 'module' THEN 1 \
                    ELSE 2 \
                  END ASC, \
                  id ASC \
         LIMIT 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let row = if let Some(kind) = required_kind {
        stmt.query_row(params![normalized_path, kind], map_entity_row)
            .optional()?
    } else {
        stmt.query_row(params![normalized_path], map_entity_row)
            .optional()?
    };
    Ok(row)
}

fn project_relative_path(project_root: &Path, normalized_path: &Path) -> Result<String> {
    let root = project_root.canonicalize()?;
    let relative = normalized_path.strip_prefix(&root).map_err(|_| {
        StorageError::InvalidSourcePath(format!(
            "{} is not under project root {}",
            normalized_path.display(),
            root.display()
        ))
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Err(StorageError::InvalidSourcePath(
                        "source path is not valid UTF-8".to_owned(),
                    ));
                };
                parts.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(StorageError::InvalidSourcePath(format!(
                    "{} is not a project-relative source path",
                    normalized_path.display()
                )));
            }
        }
    }
    Ok(parts.join("/"))
}

fn normalize_lookup_path(project_root: &Path, file: &str) -> Result<PathBuf> {
    let root = project_root.canonicalize()?;
    let input = Path::new(file);
    let candidate = if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    };
    let lexical = normalize_lexically(&candidate);
    if !lexical.starts_with(&root) {
        return Err(StorageError::InvalidSourcePath(format!(
            "{file:?} escapes project root {}",
            root.display()
        )));
    }
    Ok(lexical)
}

fn file_content_hash(path: &Path) -> std::io::Result<String> {
    fs::read(path).map(|bytes| blake3::hash(&bytes).to_hex().to_string())
}

/// Live blake3 (hex) of an entity's containing file, read at call time, by the
/// contract definition (whole-file, raw bytes). Returns `None` when the file is
/// missing/unreadable (deleted/renamed) — a stale signal, not an error. The
/// stored `entities.content_hash` is NOT used: for function entities it is a
/// span-scoped, LF-normalized hash, and even a stored whole-file hash reflects
/// the last analyze rather than current disk. `source_file_path` is resolved
/// against `project_root` when relative (file entities store absolute canonical
/// paths; some inputs are project-relative).
#[must_use]
pub fn current_file_hash(project_root: &Path, source_file_path: &str) -> Option<String> {
    let p = Path::new(source_file_path);
    let path = if p.is_absolute() {
        p.to_path_buf()
    } else {
        project_root.join(p)
    };
    match file_content_hash(&path) {
        Ok(hash) => Some(hash),
        // A missing file is the routine stale case (deleted/renamed) — stay
        // silent. Any other IO kind (PermissionDenied, a permission misconfig,
        // an IO fault) would otherwise report every fact stale forever with no
        // breadcrumb. Warn with the path AND the kind so an operator can act,
        // but still return `None`: the freshness contract is `None` = stale,
        // never an error.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                error_kind = ?err.kind(),
                "current_file_hash: source file unreadable; reporting stale (absent freshness hash)"
            );
            None
        }
    }
}

fn resolved_language(
    requested: &str,
    plugin_id: &str,
    properties_json: &str,
    path: &Path,
) -> String {
    if let Some(language) = stored_language(properties_json) {
        return language;
    }
    if plugin_id != "core" {
        return plugin_id.to_owned();
    }
    if let Some(inferred) = language_for_extension(path) {
        return inferred;
    }
    requested.trim().to_owned()
}

fn stored_language(properties_json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(properties_json)
        .ok()?
        .get("language")?
        .as_str()
        .map(str::trim)
        .filter(|language| !language.is_empty())
        .map(str::to_owned)
}

fn language_for_extension(path: &Path) -> Option<String> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("py") => Some("python".to_owned()),
        Some("rs") => Some("rust".to_owned()),
        Some("js") => Some("javascript".to_owned()),
        Some("ts") => Some("typescript".to_owned()),
        Some(extension) => Some(extension.to_owned()),
        None => None,
    }
}

/// Return the subset of `candidates` whose `id` appears in `entities`. Used by
/// the inferred-edge dispatch path to pre-filter LLM-proposed `to_id` values
/// before they reach the writer-actor's FK-protected INSERT (clarion-df58379de4).
/// Empty input is handled cheaply; large inputs are chunked to stay under the
/// default `SQLite` parameter cap (32766 placeholders per statement).
pub fn existing_entity_ids(conn: &Connection, candidates: &[String]) -> Result<HashSet<String>> {
    if candidates.is_empty() {
        return Ok(HashSet::new());
    }
    let mut found = HashSet::with_capacity(candidates.len());
    for chunk in candidates.chunks(500) {
        let placeholders = std::iter::repeat_n("?", chunk.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("SELECT id FROM entities WHERE id IN ({placeholders})");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(chunk.iter()), |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            found.insert(row?);
        }
    }
    Ok(found)
}

/// Federation-surface visibility of a single entity: whether it exists at all
/// and, if so, whether it carries a `briefing_blocked` marker. Read via the
/// generated `briefing_blocked` column in one point lookup (cheaper than
/// loading the whole row). Federation read surfaces use this to translate a
/// missing entity to 404 and a blocked entity to a refusal, mirroring the
/// file-content surface's `briefing_blocked` 403.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityVisibility {
    /// No entity row with this id.
    NotFound,
    /// The entity exists but is briefing-blocked (carries the reason).
    Blocked(String),
    /// The entity exists and may be exposed.
    Visible,
}

/// Look up an entity's [`EntityVisibility`] by id.
///
/// # Errors
///
/// Returns [`StorageError::Sqlite`] if the query fails.
pub fn entity_visibility(conn: &Connection, id: &str) -> Result<EntityVisibility> {
    let row: Option<Option<String>> = conn
        .query_row(
            "SELECT briefing_blocked FROM entities WHERE id = ?1",
            params![id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?;
    Ok(match row {
        None => EntityVisibility::NotFound,
        Some(None) => EntityVisibility::Visible,
        Some(Some(reason)) => EntityVisibility::Blocked(reason),
    })
}

pub fn entity_at_line(
    conn: &Connection,
    source_file_path: &str,
    line: i64,
) -> Result<Option<EntityRow>> {
    if line <= 0 {
        return Err(StorageError::InvalidQuery(
            "line must be a positive one-based integer".to_owned(),
        ));
    }
    let sql = format!(
        "SELECT {ENTITY_COLUMNS} \
         FROM entities \
         WHERE source_file_path = ?1 \
           AND source_line_start IS NOT NULL \
           AND source_line_end IS NOT NULL \
           AND source_line_start <= ?2 \
           AND source_line_end >= ?2 \
         ORDER BY (source_line_end - source_line_start) ASC, \
                  CASE kind \
                    WHEN 'function' THEN 0 \
                    WHEN 'class' THEN 1 \
                    WHEN 'module' THEN 2 \
                    ELSE 3 \
                  END ASC, \
                  id ASC \
         LIMIT 1"
    );
    conn.query_row(&sql, params![source_file_path, line], map_entity_row)
        .optional()
        .map_err(StorageError::from)
}

/// Every entity whose source span contains `line` in `source_file_path`,
/// innermost first.
///
/// Same ordering as [`entity_at_line`] (smallest span first, then a stable
/// kind/id tie-break) but without the `LIMIT 1`, so the caller sees the full
/// containing set: the winner is the first row, and any later row sharing the
/// winner's span length is a genuine ambiguity alternative (overlapping
/// entities at the same granularity), while strictly larger spans are the
/// nesting stack. Read-only.
///
/// # Errors
///
/// Returns [`StorageError::InvalidQuery`] for a non-positive `line`, or a
/// `SQLite` error if the query fails.
pub fn entities_containing_line(
    conn: &Connection,
    source_file_path: &str,
    line: i64,
) -> Result<Vec<EntityRow>> {
    if line <= 0 {
        return Err(StorageError::InvalidQuery(
            "line must be a positive one-based integer".to_owned(),
        ));
    }
    let sql = format!(
        "SELECT {ENTITY_COLUMNS} \
         FROM entities \
         WHERE source_file_path = ?1 \
           AND source_line_start IS NOT NULL \
           AND source_line_end IS NOT NULL \
           AND source_line_start <= ?2 \
           AND source_line_end >= ?2 \
         ORDER BY (source_line_end - source_line_start) ASC, \
                  CASE kind \
                    WHEN 'function' THEN 0 \
                    WHEN 'class' THEN 1 \
                    WHEN 'module' THEN 2 \
                    ELSE 3 \
                  END ASC, \
                  id ASC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![source_file_path, line], map_entity_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The chain of ancestor entities of `entity_id`, immediate parent first up
/// to the root (module) entity, following each row's `parent_id`.
///
/// Used to render the authoritative containing stack for `entity_at`
/// (module → class → function) independent of span arithmetic. The walk is
/// bounded by `MAX_ANCESTOR_DEPTH` so a malformed `parent_id` cycle cannot
/// loop forever. Read-only.
///
/// # Errors
///
/// Returns a `SQLite` error if a lookup fails. A dangling `parent_id` (parent
/// row absent) simply ends the chain.
pub fn ancestor_chain(conn: &Connection, entity_id: &str) -> Result<Vec<EntityRow>> {
    let mut chain = Vec::new();
    let mut current = entity_by_id(conn, entity_id)?;
    let mut depth = 0;
    while let Some(row) = current {
        let Some(parent_id) = row.parent_id.clone() else {
            break;
        };
        depth += 1;
        if depth > MAX_ANCESTOR_DEPTH {
            break;
        }
        let parent = entity_by_id(conn, &parent_id)?;
        if let Some(parent_row) = &parent {
            chain.push(parent_row.clone());
        }
        current = parent;
    }
    Ok(chain)
}

/// Upper bound on the parent-id ancestor walk in [`ancestor_chain`]. Real
/// Python nesting is shallow; this only guards against a malformed cycle.
const MAX_ANCESTOR_DEPTH: usize = 64;

pub fn find_entities(
    conn: &Connection,
    pattern: &str,
    limit: usize,
    offset: usize,
    kind: Option<&str>,
) -> Result<Vec<EntityRow>> {
    if pattern.trim().is_empty() {
        return Err(StorageError::InvalidQuery(
            "entity search pattern must not be blank".to_owned(),
        ));
    }
    // The `kind` filter is an optional exact-match on `entities.kind`. Kinds are
    // plugin-owned (ADR-003/ADR-022), so we don't validate against a hardcoded
    // allowlist — an unknown kind simply matches no rows. Reject only a blank
    // string, which is never a real kind and signals a malformed request.
    if let Some(kind) = kind
        && kind.trim().is_empty()
    {
        return Err(StorageError::InvalidQuery(
            "entity search kind filter must not be blank".to_owned(),
        ));
    }
    let limit = limit.clamp(1, 100);
    let limit_i64 = i64::try_from(limit)
        .map_err(|_| StorageError::InvalidQuery("entity search limit is too large".to_owned()))?;
    let offset_i64 = i64::try_from(offset)
        .map_err(|_| StorageError::InvalidQuery("entity search offset is too large".to_owned()))?;
    if is_fts_safe(pattern) {
        let kind_clause = if kind.is_some() {
            "AND e.kind = ?4 "
        } else {
            ""
        };
        let sql = format!(
            "SELECT e.{columns} \
             FROM entity_fts f \
             JOIN entities e ON e.id = f.entity_id \
             WHERE entity_fts MATCH ?1 {kind_clause}\
             ORDER BY bm25(entity_fts), e.id \
             LIMIT ?2 OFFSET ?3",
            columns = ENTITY_COLUMNS.replace(", ", ", e.")
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = match kind {
            Some(kind) => stmt.query_map(
                params![pattern, limit_i64, offset_i64, kind],
                map_entity_row,
            )?,
            None => stmt.query_map(params![pattern, limit_i64, offset_i64], map_entity_row)?,
        };
        return rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(StorageError::from);
    }

    let like = format!("%{}%", escape_like(pattern));
    let kind_clause = if kind.is_some() { "AND kind = ?4 " } else { "" };
    let sql = format!(
        "SELECT {ENTITY_COLUMNS} \
         FROM entities \
         WHERE (id LIKE ?1 ESCAPE '\\' \
            OR name LIKE ?1 ESCAPE '\\' \
            OR short_name LIKE ?1 ESCAPE '\\' \
            OR COALESCE(summary, '') LIKE ?1 ESCAPE '\\') {kind_clause}\
         ORDER BY id \
         LIMIT ?2 OFFSET ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = match kind {
        Some(kind) => stmt.query_map(params![like, limit_i64, offset_i64, kind], map_entity_row)?,
        None => stmt.query_map(params![like, limit_i64, offset_i64], map_entity_row)?,
    };
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

/// Collect an entity-row iterator, stopping at `scan_cap`. Returns the rows plus
/// `scan_truncated` (true when the cap was hit and more rows existed). Used by
/// the WS5 faceted-search helpers, which apply scope + pagination in the read
/// layer over the materialised candidate set.
fn collect_capped(
    rows: impl Iterator<Item = rusqlite::Result<EntityRow>>,
    scan_cap: usize,
) -> Result<(Vec<EntityRow>, bool)> {
    let mut out = Vec::new();
    let mut truncated = false;
    for row in rows {
        if out.len() >= scan_cap {
            truncated = true;
            break;
        }
        out.push(row.map_err(StorageError::from)?);
    }
    Ok((out, truncated))
}

/// Faceted catalog query: entities of a plugin-declared `kind`, ordered by id,
/// materialised up to `scan_cap`. Returns `(rows, scan_truncated)`. Kinds are
/// plugin-owned (ADR-003/ADR-022); an unknown kind matches no rows. A blank kind
/// is rejected.
pub fn entities_by_kind(
    conn: &Connection,
    kind: &str,
    scan_cap: usize,
) -> Result<(Vec<EntityRow>, bool)> {
    if kind.trim().is_empty() {
        return Err(StorageError::InvalidQuery(
            "kind filter must not be blank".to_owned(),
        ));
    }
    let limit = i64::try_from(scan_cap.saturating_add(1)).unwrap_or(i64::MAX);
    let sql = format!("SELECT {ENTITY_COLUMNS} FROM entities WHERE kind = ?1 ORDER BY id LIMIT ?2");
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![kind, limit], map_entity_row)?;
    collect_capped(rows, scan_cap)
}

/// Faceted catalog query: entities carrying `tag` (any plugin's
/// `entity_tags.tag`), ordered by id, materialised up to `scan_cap`. Returns
/// `(rows, scan_truncated)`. A blank tag is rejected; an unknown tag matches no
/// rows (the honest-empty case the read layer surfaces).
pub fn entities_by_tag(
    conn: &Connection,
    tag: &str,
    scan_cap: usize,
) -> Result<(Vec<EntityRow>, bool)> {
    if tag.trim().is_empty() {
        return Err(StorageError::InvalidQuery(
            "tag must not be blank".to_owned(),
        ));
    }
    let limit = i64::try_from(scan_cap.saturating_add(1)).unwrap_or(i64::MAX);
    let sql = format!(
        "SELECT {ENTITY_COLUMNS} FROM entities \
         WHERE id IN (SELECT entity_id FROM entity_tags WHERE tag = ?1) \
         ORDER BY id LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![tag, limit], map_entity_row)?;
    collect_capped(rows, scan_cap)
}

/// Faceted catalog query: entities that carry a Wardline taint fact, ordered by
/// id, materialised up to `scan_cap`. Returns `(rows, scan_truncated)`. The
/// `wardline_json` blob is opaque to storage; tier/group filtering is the read
/// layer's concern (it fetches blobs via [`crate::get_taint_facts`]).
pub fn entities_with_wardline_facts(
    conn: &Connection,
    scan_cap: usize,
) -> Result<(Vec<EntityRow>, bool)> {
    let limit = i64::try_from(scan_cap.saturating_add(1)).unwrap_or(i64::MAX);
    let sql = format!(
        "SELECT {ENTITY_COLUMNS} FROM entities \
         WHERE id IN (SELECT entity_id FROM wardline_taint_facts) \
         ORDER BY id LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![limit], map_entity_row)?;
    collect_capped(rows, scan_cap)
}

/// Faceted catalog query: entities carrying a non-null `git_churn_count`,
/// ordered by churn descending then id, materialised up to `scan_cap`. Returns
/// `(rows, scan_truncated)` and the churn count alongside each row. The pipeline
/// does not populate `git_churn_count` in v1.0, so this is honest-empty in
/// practice — the read layer surfaces the missing signal.
pub fn entities_by_churn(
    conn: &Connection,
    scan_cap: usize,
) -> Result<(Vec<(EntityRow, i64)>, bool)> {
    let limit = i64::try_from(scan_cap.saturating_add(1)).unwrap_or(i64::MAX);
    let sql = format!(
        "SELECT {ENTITY_COLUMNS}, git_churn_count FROM entities \
         WHERE git_churn_count IS NOT NULL \
         ORDER BY git_churn_count DESC, id LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![limit])?;
    let mut out = Vec::new();
    let mut truncated = false;
    while let Some(row) = rows.next()? {
        if out.len() >= scan_cap {
            truncated = true;
            break;
        }
        let entity = map_entity_row(row)?;
        let churn: i64 = row.get(15)?;
        out.push((entity, churn));
    }
    Ok((out, truncated))
}

pub fn call_edges_targeting(
    conn: &Connection,
    target_id: &str,
    max_confidence: EdgeConfidence,
) -> Result<Vec<CallEdgeMatch>> {
    let mut matches = Vec::new();
    let mut seen = BTreeSet::new();

    let mut direct = conn.prepare(
        "SELECT from_id, to_id, confidence, source_file_id, \
                source_byte_start, source_byte_end, properties \
         FROM edges \
         WHERE kind = 'calls' AND to_id = ?1 \
         ORDER BY from_id, to_id, source_byte_start, source_byte_end",
    )?;
    for row in direct.query_map(params![target_id], map_stored_call_edge)? {
        let edge = row?;
        if confidence_allowed(edge.confidence, max_confidence) {
            push_call_match(&mut matches, &mut seen, &edge, target_id.to_owned());
        }
    }

    if max_confidence >= EdgeConfidence::Ambiguous {
        let mut ambiguous = conn.prepare(
            "SELECT from_id, to_id, confidence, source_file_id, \
                    source_byte_start, source_byte_end, properties \
             FROM edges \
             WHERE kind = 'calls' \
               AND confidence = 'ambiguous' \
               AND properties IS NOT NULL \
             ORDER BY from_id, to_id, source_byte_start, source_byte_end",
        )?;
        for row in ambiguous.query_map([], map_stored_call_edge)? {
            let edge = row?;
            if edge.candidate_ids().contains(target_id) {
                push_call_match(&mut matches, &mut seen, &edge, target_id.to_owned());
            }
        }
    }

    Ok(matches)
}

pub fn call_edges_from(
    conn: &Connection,
    from_id: &str,
    max_confidence: EdgeConfidence,
) -> Result<Vec<CallEdgeMatch>> {
    let mut matches = Vec::new();
    let mut seen = BTreeSet::new();
    let mut stmt = conn.prepare(
        "SELECT from_id, to_id, confidence, source_file_id, \
                source_byte_start, source_byte_end, properties \
         FROM edges \
         WHERE kind = 'calls' AND from_id = ?1 \
         ORDER BY from_id, to_id, source_byte_start, source_byte_end",
    )?;
    for row in stmt.query_map(params![from_id], map_stored_call_edge)? {
        let edge = row?;
        if !confidence_allowed(edge.confidence, max_confidence) {
            continue;
        }
        if edge.confidence == EdgeConfidence::Ambiguous
            && max_confidence >= EdgeConfidence::Ambiguous
        {
            let mut targets = edge.candidate_ids();
            targets.insert(edge.stored_to_id.clone());
            for target in targets {
                push_call_match(&mut matches, &mut seen, &edge, target);
            }
        } else {
            push_call_match(&mut matches, &mut seen, &edge, edge.stored_to_id.clone());
        }
    }
    Ok(matches)
}

pub fn unresolved_call_sites_for_caller(
    conn: &Connection,
    caller_id: &str,
    limit: usize,
) -> Result<Vec<UnresolvedCallSiteRow>> {
    let limit_i64 = i64::try_from(limit.clamp(1, 500)).map_err(|_| {
        StorageError::InvalidQuery("unresolved call-site limit is too large".to_owned())
    })?;
    let mut stmt = conn.prepare(
        "SELECT u.caller_entity_id, u.caller_content_hash, u.site_key, u.site_ordinal, \
                u.source_file_id, u.source_byte_start, u.source_byte_end, u.callee_expr \
         FROM entity_unresolved_call_sites u \
         JOIN entities caller ON caller.id = u.caller_entity_id \
         WHERE u.caller_entity_id = ?1 \
           AND caller.content_hash = u.caller_content_hash \
         ORDER BY u.site_ordinal, u.site_key \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![caller_id, limit_i64], map_unresolved_call_site_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

pub fn unresolved_callers_for_target(
    conn: &Connection,
    target: &EntityRow,
    limit: usize,
) -> Result<Vec<UnresolvedCallSiteRow>> {
    let limit_i64 = i64::try_from(limit.clamp(1, 500)).map_err(|_| {
        StorageError::InvalidQuery("unresolved caller limit is too large".to_owned())
    })?;
    let target_short = target
        .short_name
        .rsplit('.')
        .next()
        .unwrap_or(&target.short_name);
    let suffix = format!("%.{}", escape_like(target_short));
    let mut stmt = conn.prepare(
        "SELECT u.caller_entity_id, u.caller_content_hash, u.site_key, u.site_ordinal, \
                u.source_file_id, u.source_byte_start, u.source_byte_end, u.callee_expr \
         FROM entity_unresolved_call_sites u \
         JOIN entities caller ON caller.id = u.caller_entity_id \
         WHERE caller.content_hash = u.caller_content_hash \
           AND (u.callee_expr = ?1 \
             OR u.callee_expr = ?2 \
             OR u.callee_expr LIKE ?3 ESCAPE '\\') \
         ORDER BY CASE WHEN caller.source_file_id = ?4 THEN 0 ELSE 1 END, \
                  u.caller_entity_id, u.site_ordinal, u.site_key \
         LIMIT ?5",
    )?;
    let rows = stmt.query_map(
        params![
            target_short,
            target.name,
            suffix,
            target.source_file_id,
            limit_i64,
        ],
        map_unresolved_call_site_row,
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

pub fn reference_edges_for_entity(
    conn: &Connection,
    entity_id: &str,
    direction: ReferenceDirection,
) -> Result<Vec<ReferenceEdgeMatch>> {
    directed_edges_for_entity(conn, entity_id, direction, "references")
}

/// `imports` edges (module → module). Direction `In` answers "who imports this
/// module" — the reverse-import lookup neighborhood previously could not serve
/// because it only read `references` edges (clarion-79d0ff6e14).
pub fn import_edges_for_entity(
    conn: &Connection,
    entity_id: &str,
    direction: ReferenceDirection,
) -> Result<Vec<ReferenceEdgeMatch>> {
    directed_edges_for_entity(conn, entity_id, direction, "imports")
}

fn directed_edges_for_entity(
    conn: &Connection,
    entity_id: &str,
    direction: ReferenceDirection,
    kind: &str,
) -> Result<Vec<ReferenceEdgeMatch>> {
    let sql = match direction {
        ReferenceDirection::In => {
            "SELECT from_id, confidence, source_file_id, source_byte_start, source_byte_end \
             FROM edges \
             WHERE kind = ?1 AND to_id = ?2 \
             ORDER BY from_id, source_byte_start, source_byte_end"
        }
        ReferenceDirection::Out => {
            "SELECT to_id, confidence, source_file_id, source_byte_start, source_byte_end \
             FROM edges \
             WHERE kind = ?1 AND from_id = ?2 \
             ORDER BY to_id, source_byte_start, source_byte_end"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![kind, entity_id], map_reference_edge_match)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

/// Aggregate the `references` edges of every entity transitively contained in
/// `module_id` (via `contains`), for module-altitude reference rollup and the
/// reverse-import lookup ("who imports this module / contract?").
///
/// A Python `from pkg.contracts import RunStatus` is recorded as a `references`
/// edge to the *class*, not the module — so a module's OWN reference edges are
/// almost always empty and "who references this module?" answered `[]`
/// (clarion-79d0ff6e14). This rolls the contained symbols' edges up to the
/// module: direction `In` lists external referencers (who imports a contained
/// symbol), `Out` lists what contained symbols reference outside the module.
///
/// Intra-module edges (both endpoints contained in the same module) are
/// excluded — they are internal wiring, not a reverse-import answer. Results
/// are ordered deterministically. The recursive CTE uses `UNION` (not `UNION
/// ALL`), so a pathological `contains` cycle terminates instead of looping.
pub fn module_reference_rollup(
    conn: &Connection,
    module_id: &str,
    direction: ReferenceDirection,
) -> Result<Vec<RolledUpReferenceEdge>> {
    // Column 0 is always the far-side neighbor, column 1 the contained `via`
    // symbol, so `map_rolled_up_reference_edge` is direction-agnostic.
    let sql = match direction {
        ReferenceDirection::In => {
            "WITH RECURSIVE contained(id) AS ( \
                 SELECT ?1 \
                 UNION \
                 SELECT child.to_id FROM edges child \
                 JOIN contained ON contained.id = child.from_id \
                 WHERE child.kind = 'contains' \
             ) \
             SELECT ed.from_id, ed.to_id, ed.confidence, ed.source_file_id, \
                    ed.source_byte_start, ed.source_byte_end \
             FROM edges ed \
             JOIN contained ON contained.id = ed.to_id \
             WHERE ed.kind = 'references' \
               AND ed.from_id NOT IN (SELECT id FROM contained) \
             ORDER BY ed.from_id, ed.to_id, ed.source_byte_start, ed.source_byte_end"
        }
        ReferenceDirection::Out => {
            "WITH RECURSIVE contained(id) AS ( \
                 SELECT ?1 \
                 UNION \
                 SELECT child.to_id FROM edges child \
                 JOIN contained ON contained.id = child.from_id \
                 WHERE child.kind = 'contains' \
             ) \
             SELECT ed.to_id, ed.from_id, ed.confidence, ed.source_file_id, \
                    ed.source_byte_start, ed.source_byte_end \
             FROM edges ed \
             JOIN contained ON contained.id = ed.from_id \
             WHERE ed.kind = 'references' \
               AND ed.to_id NOT IN (SELECT id FROM contained) \
             ORDER BY ed.to_id, ed.from_id, ed.source_byte_start, ed.source_byte_end"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![module_id], map_rolled_up_reference_edge)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

fn map_rolled_up_reference_edge(row: &Row<'_>) -> rusqlite::Result<RolledUpReferenceEdge> {
    let raw_confidence: String = row.get(2)?;
    Ok(RolledUpReferenceEdge {
        neighbor_id: row.get(0)?,
        via_id: row.get(1)?,
        confidence: parse_confidence(&raw_confidence)?,
        source_file_id: row.get(3)?,
        source_byte_start: row.get(4)?,
        source_byte_end: row.get(5)?,
    })
}

pub fn module_dependency_edges(
    conn: &Connection,
    edge_types: &[&str],
) -> Result<Vec<ModuleDependencyEdge>> {
    if edge_types.is_empty() {
        return Ok(Vec::new());
    }

    // v0.1 assumes a wipe-and-rerun analyze workflow, so clustering reads the
    // whole static graph. v0.2 incremental analyze needs run-scoped edge
    // provenance before this helper can filter by current run.
    let ancestors = module_ancestor_map(conn)?;
    let placeholders = std::iter::repeat_n("?", edge_types.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT from_id, to_id, kind, confidence, properties \
         FROM edges \
         WHERE kind IN ({placeholders}) \
           AND confidence != 'inferred' \
         ORDER BY from_id, to_id, kind",
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(edge_types.iter().copied()), |row| {
        let raw_confidence: String = row.get(3)?;
        Ok(StoredDependencyEdge {
            from_id: row.get(0)?,
            to_id: row.get(1)?,
            kind: row.get(2)?,
            confidence: parse_confidence(&raw_confidence)?,
            properties_json: row.get(4)?,
        })
    })?;

    let mut grouped: BTreeMap<(String, String), (u64, BTreeSet<String>)> = BTreeMap::new();
    for row in rows {
        let edge = row?;
        let Some(from_modules) = ancestors.get(&edge.from_id) else {
            continue;
        };
        for target_id in dependency_edge_target_ids(&edge) {
            let Some(to_modules) = ancestors.get(&target_id) else {
                continue;
            };
            for from_module_id in from_modules {
                for to_module_id in to_modules {
                    if from_module_id == to_module_id {
                        continue;
                    }
                    let (reference_count, edge_kinds) = grouped
                        .entry((from_module_id.clone(), to_module_id.clone()))
                        .or_insert_with(|| (0, BTreeSet::new()));
                    *reference_count += 1;
                    edge_kinds.insert(edge.kind.clone());
                }
            }
        }
    }

    Ok(grouped
        .into_iter()
        .map(
            |((from_module_id, to_module_id), (reference_count, edge_kinds))| {
                ModuleDependencyEdge {
                    from_module_id,
                    to_module_id,
                    reference_count,
                    edge_kinds: edge_kinds.into_iter().collect(),
                }
            },
        )
        .collect())
}

fn module_ancestor_map(conn: &Connection) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE module_ancestors(entity_id, module_id, depth) AS ( \
             SELECT id, id, 0 FROM entities WHERE kind = 'module' \
             UNION ALL \
             SELECT child.to_id, module_ancestors.module_id, module_ancestors.depth + 1 \
             FROM edges child \
             JOIN module_ancestors ON module_ancestors.entity_id = child.from_id \
             WHERE child.kind = 'contains' \
               AND module_ancestors.depth < ?1 \
         ) \
         SELECT DISTINCT entity_id, module_id \
         FROM module_ancestors \
         ORDER BY entity_id, module_id",
    )?;
    let rows = stmt.query_map(params![MODULE_ANCESTOR_MAX_DEPTH], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut ancestors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        let (entity_id, module_id) = row?;
        ancestors.entry(entity_id).or_default().insert(module_id);
    }
    Ok(ancestors)
}

fn dependency_edge_target_ids(edge: &StoredDependencyEdge) -> BTreeSet<String> {
    let mut targets = BTreeSet::from([edge.to_id.clone()]);
    if edge.kind == "calls" && edge.confidence == EdgeConfidence::Ambiguous {
        targets.extend(candidate_ids(edge.properties_json.as_deref()));
    }
    targets
}

pub fn subsystem_members(conn: &Connection, subsystem_id: &str) -> Result<Vec<SubsystemMember>> {
    let mut stmt = conn.prepare(
        "SELECT entities.id, entities.name, entities.source_file_path \
         FROM edges \
         JOIN entities ON entities.id = edges.from_id \
         WHERE edges.kind = 'in_subsystem' \
           AND edges.to_id = ?1 \
           AND entities.kind = 'module' \
         ORDER BY entities.name, entities.id",
    )?;
    let rows = stmt.query_map(params![subsystem_id], |row| {
        Ok(SubsystemMember {
            id: row.get(0)?,
            name: row.get(1)?,
            source_file_path: row.get(2)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

/// The subsystem an entity belongs to, plus the module the membership was
/// resolved through (the entity itself when it is a module).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitySubsystem {
    pub subsystem_id: String,
    pub via_module_id: String,
}

/// Resolve the subsystem an arbitrary entity belongs to — the reverse of
/// [`subsystem_members`].
///
/// `in_subsystem` edges connect *modules* to subsystems, so for a non-module
/// entity (function, class, …) this walks up `contains` edges to the nearest
/// module ancestor and follows that module's `in_subsystem` edge. A module
/// entity resolves directly (depth 0). Returns the nearest match, or `None` if
/// the entity has no module ancestor that is assigned to a subsystem.
pub fn subsystem_of_entity(conn: &Connection, entity_id: &str) -> Result<Option<EntitySubsystem>> {
    conn.query_row(
        "WITH RECURSIVE ancestors(id, depth) AS ( \
             SELECT ?1, 0 \
             UNION ALL \
             SELECT parent.from_id, ancestors.depth + 1 \
             FROM edges parent \
             JOIN ancestors ON parent.to_id = ancestors.id \
             WHERE parent.kind = 'contains' AND ancestors.depth < ?2 \
         ) \
         SELECT m.id, sub.to_id \
         FROM ancestors \
         JOIN entities m ON m.id = ancestors.id AND m.kind = 'module' \
         JOIN edges sub ON sub.kind = 'in_subsystem' AND sub.from_id = m.id \
         JOIN entities s ON s.id = sub.to_id AND s.kind = 'subsystem' \
         ORDER BY ancestors.depth, sub.to_id \
         LIMIT 1",
        params![entity_id, MODULE_ANCESTOR_MAX_DEPTH],
        |row| {
            Ok(EntitySubsystem {
                via_module_id: row.get(0)?,
                subsystem_id: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(StorageError::from)
}

/// Resolve the module that contains `entity_id`: the nearest `module`-kind
/// ancestor reached by walking `contains` edges upward, or the entity itself
/// when it is already a module (depth 0).
///
/// Used to lift a reverse-import (`who imports this`) result to module altitude
/// (clarion-79d0ff6e14). A `references` edge is recorded against the importing
/// *symbol* (`from pkg.contracts import X` binds to the class `X`), but the
/// reverse-import contract names importing *modules* — so a consumer resolves
/// each importer to its module here. Returns `None` for a symbol with no module
/// ancestor within `MODULE_ANCESTOR_MAX_DEPTH`.
pub fn containing_module_id(conn: &Connection, entity_id: &str) -> Result<Option<String>> {
    conn.query_row(
        "WITH RECURSIVE ancestors(id, depth) AS ( \
             SELECT ?1, 0 \
             UNION ALL \
             SELECT parent.from_id, ancestors.depth + 1 \
             FROM edges parent \
             JOIN ancestors ON parent.to_id = ancestors.id \
             WHERE parent.kind = 'contains' AND ancestors.depth < ?2 \
         ) \
         SELECT m.id \
         FROM ancestors \
         JOIN entities m ON m.id = ancestors.id AND m.kind = 'module' \
         ORDER BY ancestors.depth \
         LIMIT 1",
        params![entity_id, MODULE_ANCESTOR_MAX_DEPTH],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(StorageError::from)
}

pub fn subsystem_for_member(conn: &Connection, module_id: &str) -> Result<Option<String>> {
    // Reserved for v0.2 neighborhood / issues_for enrichment. v0.1's MCP
    // surface exposes subsystem_members, but keeping this inverse lookup here
    // preserves the query contract and tests until those callers land.
    conn.query_row(
        "SELECT edges.to_id \
         FROM edges \
         JOIN entities ON entities.id = edges.to_id \
         WHERE edges.kind = 'in_subsystem' \
           AND edges.from_id = ?1 \
           AND entities.kind = 'subsystem' \
         ORDER BY edges.to_id \
         LIMIT 1",
        params![module_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(StorageError::from)
}

pub fn candidate_entities_for_unresolved_sites(
    conn: &Connection,
    sites: &[UnresolvedCallSiteRow],
    limit: usize,
) -> Result<Vec<EntityRow>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for site in sites {
        for entity in candidate_entities_for_expr(conn, &site.callee_expr, limit)? {
            if seen.insert(entity.id.clone()) {
                out.push(entity);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
    }
    Ok(out)
}

pub fn contained_entity_ids(
    conn: &Connection,
    root_id: &str,
    max_entities: usize,
) -> Result<ContainedEntities> {
    let mut visited = BTreeSet::from([root_id.to_owned()]);
    let mut entity_ids = Vec::new();
    let mut stack = child_entity_ids(conn, root_id)?;
    stack.reverse();

    while let Some(entity_id) = stack.pop() {
        if !visited.insert(entity_id.clone()) {
            continue;
        }
        if entity_ids.len() >= max_entities {
            return Ok(ContainedEntities {
                entity_ids,
                truncated: true,
            });
        }
        entity_ids.push(entity_id.clone());
        let mut children = child_entity_ids(conn, &entity_id)?;
        children.reverse();
        for child in children {
            if !visited.contains(&child) {
                stack.push(child);
            }
        }
    }

    Ok(ContainedEntities {
        entity_ids,
        truncated: false,
    })
}

/// All findings recorded under `run_id`, joined to their anchoring entity's
/// source location, ordered by finding id for deterministic emission. Used by
/// the WP9-B cross-product emitter to build a `POST /api/v1/scan-results`
/// batch. Findings whose anchor entity has no `source_file_path` are returned
/// with `source_file_path: None`; the emitter skips them (Filigree requires a
/// `path`).
///
/// Findings anchored to a `briefing_blocked` entity are excluded: emission is a
/// one-way path/line egress to a sibling, and the federation read API
/// (`GET /api/v1/files`) already refuses briefing-blocked entities and omits
/// their identity fields. Without this guard the write direction would leak the
/// very path/line the read direction is engineered to withhold — e.g. a
/// secret-scanner `LMWV-SEC-SECRET-DETECTED` finding on a still-blocked
/// secret-bearing file. The filter is safe for the ADR-013 audit trail: an
/// operator override (`--allow-unredacted-secrets`) records the file as
/// `Overridden`, not `Blocked`, so its anchor entity carries no
/// `briefing_blocked` reason and the `LMWV-SEC-UNREDACTED-SECRETS-ALLOWED` audit
/// finding still emits.
pub fn findings_for_emit(conn: &Connection, run_id: &str) -> Result<Vec<FindingForEmitRow>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.rule_id, f.kind, f.severity, f.confidence, \
                f.confidence_basis, f.message, f.entity_id, f.related_entities, \
                f.supports, f.supported_by, \
                e.source_file_path, e.source_line_start, e.source_line_end \
         FROM findings f \
         JOIN entities e ON e.id = f.entity_id \
         WHERE f.run_id = ?1 \
           AND e.briefing_blocked IS NULL \
         ORDER BY f.id",
    )?;
    let rows = stmt.query_map(params![run_id], |row| {
        Ok(FindingForEmitRow {
            id: row.get(0)?,
            rule_id: row.get(1)?,
            kind: row.get(2)?,
            severity: row.get(3)?,
            confidence: row.get(4)?,
            confidence_basis: row.get(5)?,
            message: row.get(6)?,
            entity_id: row.get(7)?,
            related_entities_json: row.get(8)?,
            supports_json: row.get(9)?,
            supported_by_json: row.get(10)?,
            source_file_path: row.get(11)?,
            source_line_start: row.get(12)?,
            source_line_end: row.get(13)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

fn map_entity_row(row: &Row<'_>) -> rusqlite::Result<EntityRow> {
    Ok(EntityRow {
        id: row.get(0)?,
        plugin_id: row.get(1)?,
        kind: row.get(2)?,
        name: row.get(3)?,
        short_name: row.get(4)?,
        parent_id: row.get(5)?,
        source_file_id: row.get(6)?,
        source_file_path: row.get(7)?,
        source_byte_start: row.get(8)?,
        source_byte_end: row.get(9)?,
        source_line_start: row.get(10)?,
        source_line_end: row.get(11)?,
        properties_json: row.get(12)?,
        content_hash: row.get(13)?,
        summary_json: row.get(14)?,
    })
}

fn map_stored_call_edge(row: &Row<'_>) -> rusqlite::Result<StoredCallEdge> {
    let raw_confidence: String = row.get(2)?;
    Ok(StoredCallEdge {
        from_id: row.get(0)?,
        stored_to_id: row.get(1)?,
        confidence: parse_confidence(&raw_confidence)?,
        source_file_id: row.get(3)?,
        source_byte_start: row.get(4)?,
        source_byte_end: row.get(5)?,
        properties_json: row.get(6)?,
    })
}

fn map_unresolved_call_site_row(row: &Row<'_>) -> rusqlite::Result<UnresolvedCallSiteRow> {
    Ok(UnresolvedCallSiteRow {
        caller_entity_id: row.get(0)?,
        caller_content_hash: row.get(1)?,
        site_key: row.get(2)?,
        site_ordinal: row.get(3)?,
        source_file_id: row.get(4)?,
        source_byte_start: row.get(5)?,
        source_byte_end: row.get(6)?,
        callee_expr: row.get(7)?,
    })
}

fn map_reference_edge_match(row: &Row<'_>) -> rusqlite::Result<ReferenceEdgeMatch> {
    let raw_confidence: String = row.get(1)?;
    Ok(ReferenceEdgeMatch {
        neighbor_id: row.get(0)?,
        confidence: parse_confidence(&raw_confidence)?,
        source_file_id: row.get(2)?,
        source_byte_start: row.get(3)?,
        source_byte_end: row.get(4)?,
    })
}

fn candidate_entities_for_expr(
    conn: &Connection,
    callee_expr: &str,
    limit: usize,
) -> Result<Vec<EntityRow>> {
    let short = callee_expr.rsplit('.').next().unwrap_or(callee_expr).trim();
    if short.is_empty() {
        return Ok(Vec::new());
    }
    let suffix = format!("%.{}", escape_like(short));
    let limit_i64 = i64::try_from(limit.clamp(1, 100)).map_err(|_| {
        StorageError::InvalidQuery("candidate entity limit is too large".to_owned())
    })?;
    let sql = format!(
        "SELECT {ENTITY_COLUMNS} \
         FROM entities \
         WHERE short_name = ?1 \
            OR name = ?2 \
            OR name LIKE ?3 ESCAPE '\\' \
         ORDER BY id \
         LIMIT ?4"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        params![short, callee_expr, suffix, limit_i64],
        map_entity_row,
    )?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

pub fn child_entity_ids(conn: &Connection, entity_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT to_id \
         FROM edges \
         WHERE kind = 'contains' AND from_id = ?1 \
         ORDER BY to_id",
    )?;
    let rows = stmt.query_map(params![entity_id], |row| row.get::<_, String>(0))?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StorageError::from)
}

fn confidence_allowed(actual: EdgeConfidence, max_confidence: EdgeConfidence) -> bool {
    actual <= max_confidence
}

fn push_call_match(
    matches: &mut Vec<CallEdgeMatch>,
    seen: &mut BTreeSet<(String, String, Option<i64>, Option<i64>)>,
    edge: &StoredCallEdge,
    to_id: String,
) {
    let key = (
        edge.from_id.clone(),
        to_id.clone(),
        edge.source_byte_start,
        edge.source_byte_end,
    );
    if !seen.insert(key) {
        return;
    }
    matches.push(CallEdgeMatch {
        from_id: edge.from_id.clone(),
        to_id,
        stored_to_id: edge.stored_to_id.clone(),
        confidence: edge.confidence,
        source_file_id: edge.source_file_id.clone(),
        source_byte_start: edge.source_byte_start,
        source_byte_end: edge.source_byte_end,
        properties_json: edge.properties_json.clone(),
    });
}

fn parse_confidence(raw: &str) -> rusqlite::Result<EdgeConfidence> {
    match raw {
        "resolved" => Ok(EdgeConfidence::Resolved),
        "ambiguous" => Ok(EdgeConfidence::Ambiguous),
        "inferred" => Ok(EdgeConfidence::Inferred),
        _ => Err(rusqlite::Error::InvalidColumnType(
            2,
            "confidence".to_owned(),
            rusqlite::types::Type::Text,
        )),
    }
}

impl StoredCallEdge {
    fn candidate_ids(&self) -> BTreeSet<String> {
        candidate_ids(self.properties_json.as_deref())
    }
}

fn candidate_ids(properties_json: Option<&str>) -> BTreeSet<String> {
    properties_json
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .and_then(|value| value.get("candidates").and_then(|c| c.as_array()).cloned())
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(ToOwned::to_owned))
        .collect()
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR)),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn is_fts_safe(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|ch| ch.is_alphanumeric() || ch == '_' || ch.is_whitespace())
}

fn escape_like(pattern: &str) -> String {
    let mut escaped = String::new();
    for ch in pattern.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(test)]
mod current_file_hash_tests {
    use super::*;
    use std::io::Write;

    /// `current_file_hash` is the WHOLE-FILE blake3 of the raw bytes read live,
    /// and is distinct from the span-scoped, LF-normalized hash that function
    /// entities store in `entities.content_hash`. This is the test that closes
    /// the W.3 freshness blind spot: the contract's `current_content_hash` MUST
    /// be whole-file, never the stored span hash.
    #[test]
    fn whole_file_hash_differs_from_span_hash() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mod.py");
        // Multi-line file WITH a trailing newline so the LF-normalized span
        // join cannot accidentally equal the whole-file bytes.
        let contents = "line0\nline1\nline2\nline3\n";
        let mut f = std::fs::File::create(&file).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        drop(f);

        // Live whole-file hash via the helper (absolute path branch).
        let live = current_file_hash(dir.path(), file.to_str().unwrap()).unwrap();

        // Reference whole-file hash: blake3 of the raw bytes, exactly.
        let whole = blake3::hash(&fs::read(&file).unwrap()).to_hex().to_string();
        assert_eq!(live, whole, "current_file_hash must be whole-file blake3");

        // Span-hash formula (analyze.rs::content_hash_for_entity): read the
        // file as text, take a STRICT sub-range of lines, LF-join, blake3.
        let text = fs::read_to_string(&file).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let span = lines[1..3].join("\n"); // "line1\nline2"
        let span_hash = blake3::hash(span.as_bytes()).to_hex().to_string();
        assert_ne!(
            live, span_hash,
            "whole-file hash must differ from the span/LF-normalized hash"
        );
    }

    #[test]
    fn relative_path_resolves_against_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pkg/mod.py");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x = 1\n").unwrap();

        let live = current_file_hash(dir.path(), "pkg/mod.py").unwrap();
        let whole = blake3::hash(&fs::read(&file).unwrap()).to_hex().to_string();
        assert_eq!(live, whole);
    }

    #[test]
    fn missing_path_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(current_file_hash(dir.path(), "does/not/exist.py"), None);
    }
}
