//! Read-side query helpers used by the MCP navigation surface.

use std::collections::{BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};

use clarion_core::EdgeConfidence;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::{Result, StorageError};

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

pub fn find_entities(
    conn: &Connection,
    pattern: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<EntityRow>> {
    if pattern.trim().is_empty() {
        return Err(StorageError::InvalidQuery(
            "entity search pattern must not be blank".to_owned(),
        ));
    }
    let limit = limit.clamp(1, 100);
    let limit_i64 = i64::try_from(limit)
        .map_err(|_| StorageError::InvalidQuery("entity search limit is too large".to_owned()))?;
    let offset_i64 = i64::try_from(offset)
        .map_err(|_| StorageError::InvalidQuery("entity search offset is too large".to_owned()))?;
    if is_fts_safe(pattern) {
        let sql = format!(
            "SELECT e.{columns} \
             FROM entity_fts f \
             JOIN entities e ON e.id = f.entity_id \
             WHERE entity_fts MATCH ?1 \
             ORDER BY bm25(entity_fts), e.id \
             LIMIT ?2 OFFSET ?3",
            columns = ENTITY_COLUMNS.replace(", ", ", e.")
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![pattern, limit_i64, offset_i64], map_entity_row)?;
        return rows
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(StorageError::from);
    }

    let like = format!("%{}%", escape_like(pattern));
    let sql = format!(
        "SELECT {ENTITY_COLUMNS} \
         FROM entities \
         WHERE id LIKE ?1 ESCAPE '\\' \
            OR name LIKE ?1 ESCAPE '\\' \
            OR short_name LIKE ?1 ESCAPE '\\' \
            OR COALESCE(summary, '') LIKE ?1 ESCAPE '\\' \
         ORDER BY id \
         LIMIT ?2 OFFSET ?3"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![like, limit_i64, offset_i64], map_entity_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(StorageError::from)
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
        "SELECT caller_entity_id, caller_content_hash, site_key, site_ordinal, \
                source_file_id, source_byte_start, source_byte_end, callee_expr \
         FROM entity_unresolved_call_sites \
         WHERE caller_entity_id = ?1 \
         ORDER BY site_ordinal, site_key \
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
         WHERE u.callee_expr = ?1 \
            OR u.callee_expr = ?2 \
            OR u.callee_expr LIKE ?3 ESCAPE '\\' \
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
    let sql = match direction {
        ReferenceDirection::In => {
            "SELECT from_id, confidence, source_file_id, source_byte_start, source_byte_end \
             FROM edges \
             WHERE kind = 'references' AND to_id = ?1 \
             ORDER BY from_id, source_byte_start, source_byte_end"
        }
        ReferenceDirection::Out => {
            "SELECT to_id, confidence, source_file_id, source_byte_start, source_byte_end \
             FROM edges \
             WHERE kind = 'references' AND from_id = ?1 \
             ORDER BY to_id, source_byte_start, source_byte_end"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![entity_id], map_reference_edge_match)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
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
        self.properties_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .and_then(|value| value.get("candidates").and_then(|c| c.as_array()).cloned())
            .into_iter()
            .flatten()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect()
    }
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
