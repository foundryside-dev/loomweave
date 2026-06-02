//! WS5 inspection reads: `guidance_for`, `findings_for`, `wardline_for`.
//!
//! These are the read-side of the guidance / findings / wardline surfaces. They
//! READ composed/anchored records for one entity; authoring (WS6) is out of
//! scope. Each carries the queried entity through [`crate::entity_json`] so the
//! response is SEI-bearing (ADR-038).

use std::collections::HashSet;

use serde_json::{Value, json};

use clarion_core::McpErrorCode;
use clarion_storage::{entity_by_id, get_taint_facts, sei_for_locator, subsystem_of_entity};

use crate::ParamError;
use crate::ServerState;
use crate::catalogue::{Page, missing_signal, paginate};
use crate::{
    entity_json, flatten_storage_envelope_result, required_str, success_envelope,
    tool_error_envelope,
};

/// Bound on guidance sheets scanned per `guidance_for` call. Guidance is
/// authored, low-cardinality institutional knowledge; this only guards a
/// pathological project.
const GUIDANCE_SCAN_CAP: usize = 2000;

/// Bound on findings scanned per `findings_for` call before in-memory filtering.
const FINDINGS_SCAN_CAP: usize = 5000;

/// Default / max page size for `findings_for`.
const FINDINGS_PAGE_DEFAULT: usize = 50;
const FINDINGS_PAGE_MAX: usize = 200;

/// Default / max page size for `guidance_for`'s composed-sheet list.
const GUIDANCE_PAGE_DEFAULT: usize = 50;
const GUIDANCE_PAGE_MAX: usize = 200;

impl ServerState {
    /// `guidance_for(entity_id)` — guidance sheets applicable to the entity,
    /// composed at query time and ranked by `scope_rank` (project → function),
    /// ties broken by `authored_at` then id. Read-only composition: explicit
    /// `guides`-edge targets plus `match_rules` (path / tag / kind / subsystem /
    /// entity) resolved against the entity's facts. `wardline_group` rules are
    /// not evaluable here (the Wardline blob is opaque) and are reported, never
    /// guessed. Expired sheets are excluded. Stateless, bounded, SEI-carrying.
    pub(crate) async fn tool_guidance_for(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let page = Page::parse(arguments, GUIDANCE_PAGE_DEFAULT, GUIDANCE_PAGE_MAX)?;
        let project_root = self.project_root.clone();
        let now = (self.clock)();
        let result = self
            .readers
            .with_reader(move |conn| {
                let Some(entity) = entity_by_id(conn, &entity_id)? else {
                    return Ok(tool_error_envelope(
                        McpErrorCode::EntityNotFound,
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                };

                let facts = EntityFacts::load(conn, &entity, &project_root)?;
                let guides_targets = guides_edge_sources(conn, &entity.id)?;

                let mut wardline_group_skipped = false;
                let mut scanned = 0usize;
                let mut scan_truncated = false;
                let mut composed: Vec<ComposedSheet> = Vec::new();

                let mut stmt = conn.prepare(
                    "SELECT id, name, \
                            json_extract(properties, '$.scope_level'), \
                            scope_rank, \
                            json_extract(properties, '$.match_rules'), \
                            json_extract(properties, '$.content'), \
                            json_extract(properties, '$.expires'), \
                            json_extract(properties, '$.pinned'), \
                            json_extract(properties, '$.provenance'), \
                            json_extract(properties, '$.authored_at') \
                       FROM entities WHERE kind = 'guidance'",
                )?;
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    if scanned >= GUIDANCE_SCAN_CAP {
                        scan_truncated = true;
                        break;
                    }
                    scanned += 1;
                    let sheet = GuidanceRow::from_row(row)?;

                    // Expiry: lexical ISO-8601 compare against the server clock.
                    if sheet.expires.as_deref().is_some_and(|exp| exp < now.as_str()) {
                        continue;
                    }

                    let mut matched_by: Vec<String> = Vec::new();
                    if guides_targets.contains(&sheet.id) {
                        matched_by.push("guides_edge".to_owned());
                    }
                    if let Some(rules) = sheet.match_rules.as_ref() {
                        for rule in rules {
                            match rule_match(rule, &facts) {
                                RuleVerdict::Matched(label) => {
                                    if !matched_by.iter().any(|m| m == label) {
                                        matched_by.push(label.to_owned());
                                    }
                                }
                                RuleVerdict::Unevaluable => wardline_group_skipped = true,
                                RuleVerdict::NoMatch => {}
                            }
                        }
                    }
                    if matched_by.is_empty() {
                        continue;
                    }
                    // The sheet row IS an entity (kind=guidance); its `sei` is
                    // the sheet's own locator-independent identity, not the
                    // queried entity's.
                    let sei = sei_for_locator(conn, &sheet.id)?;
                    composed.push(ComposedSheet {
                        sheet,
                        matched_by,
                        sei,
                    });
                }

                // Rank: scope_rank ASC (NULL last), then authored_at ASC, then id.
                composed.sort_by(|a, b| {
                    a.sheet
                        .scope_rank
                        .unwrap_or(i64::MAX)
                        .cmp(&b.sheet.scope_rank.unwrap_or(i64::MAX))
                        .then_with(|| a.sheet.authored_at.cmp(&b.sheet.authored_at))
                        .then_with(|| a.sheet.id.cmp(&b.sheet.id))
                });

                let (slice, meta) = paginate(&composed, page);
                let sheets: Vec<Value> = slice.iter().map(ComposedSheet::to_json).collect();

                let mut result = json!({
                    "entity": entity_json(conn, &entity),
                    "guidance": sheets,
                    "page": meta,
                    "scanned": scanned,
                    "scan_truncated": scan_truncated,
                });
                if let Some(object) = result.as_object_mut()
                    && wardline_group_skipped
                {
                    object.insert(
                        "notes".to_owned(),
                        json!([missing_signal(
                            "wardline_group",
                            "guidance wardline_group match-rules are not evaluated here: the \
                             Wardline blob is opaque to Clarion; use wardline_for / find_by_wardline"
                        )]),
                    );
                }
                Ok(success_envelope(result))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    /// `findings_for(entity_id, filter?)` — findings anchored to the entity,
    /// optionally filtered by `kind` / `severity` / `status`. Bounded
    /// (limit/offset, total/truncated). The queried entity carries its SEI;
    /// each finding's `related_entities` are raw locator ids (references, not the
    /// primary return). Stateless.
    pub(crate) async fn tool_findings_for(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let filter = FindingFilter::parse(arguments)?;
        let page = Page::parse(arguments, FINDINGS_PAGE_DEFAULT, FINDINGS_PAGE_MAX)?;
        let result = self
            .readers
            .with_reader(move |conn| {
                let Some(entity) = entity_by_id(conn, &entity_id)? else {
                    return Ok(tool_error_envelope(
                        McpErrorCode::EntityNotFound,
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                };

                let mut stmt = conn.prepare(
                    "SELECT id, tool, rule_id, kind, severity, status, message, \
                            related_entities, confidence, created_at \
                       FROM findings WHERE entity_id = ?1 \
                      ORDER BY created_at DESC, id LIMIT ?2",
                )?;
                let cap = i64::try_from(FINDINGS_SCAN_CAP).unwrap_or(i64::MAX);
                let mut rows = stmt.query(rusqlite::params![entity.id, cap])?;
                let mut all: Vec<FindingRow> = Vec::new();
                let mut scan_truncated = false;
                while let Some(row) = rows.next()? {
                    if all.len() >= FINDINGS_SCAN_CAP {
                        scan_truncated = true;
                        break;
                    }
                    all.push(FindingRow::from_row(row)?);
                }
                let filtered: Vec<FindingRow> =
                    all.into_iter().filter(|f| filter.matches(f)).collect();

                let (slice, meta) = paginate(&filtered, page);
                let findings: Vec<Value> = slice.iter().map(FindingRow::to_json).collect();

                Ok(success_envelope(json!({
                    "entity": entity_json(conn, &entity),
                    "findings": findings,
                    "filter": filter.to_json(),
                    "page": meta,
                    "scan_truncated": scan_truncated,
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    /// `wardline_for(entity_id)` — the Wardline metadata recorded for the entity
    /// (declared tier, groups, boundary contracts), returned **verbatim**: the
    /// `wardline_json` blob is opaque to Clarion (federation opacity contract).
    /// `result_kind` is `present` when a fact exists, else `no_facts` with a
    /// missing-signal note — taint facts are populated via Filigree Flow-B
    /// (`POST /api/wardline/taint-facts`), so locally-empty is honest, not an
    /// error. Stateless, SEI-carrying.
    pub(crate) async fn tool_wardline_for(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let result = self
            .readers
            .with_reader(move |conn| {
                let Some(entity) = entity_by_id(conn, &entity_id)? else {
                    return Ok(tool_error_envelope(
                        McpErrorCode::EntityNotFound,
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                };

                let facts = get_taint_facts(conn, std::slice::from_ref(&entity.id))?;
                let entity_block = entity_json(conn, &entity);
                let response = match facts.into_iter().next() {
                    Some(fact) => {
                        // Opaque: parse as JSON for structured return, but if the
                        // blob is not JSON, return it as a raw string rather than
                        // failing — Clarion never depends on its shape.
                        let wardline = serde_json::from_str::<Value>(&fact.wardline_json)
                            .unwrap_or(Value::String(fact.wardline_json));
                        json!({
                            "entity": entity_block,
                            "result_kind": "present",
                            "wardline": wardline,
                        })
                    }
                    None => json!({
                        "entity": entity_block,
                        "result_kind": "no_facts",
                        "wardline": Value::Null,
                        "signal": missing_signal(
                            "wardline_taint_facts",
                            "no Wardline taint fact is recorded for this entity; facts are \
                             populated via Filigree Flow-B (POST /api/wardline/taint-facts)"
                        ),
                    }),
                };
                Ok(success_envelope(response))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }
}

/// Entity facts a guidance `match_rules` evaluation needs.
struct EntityFacts {
    kind: String,
    rel_path: Option<String>,
    tags: HashSet<String>,
    subsystem_id: Option<String>,
    entity_id: String,
}

impl EntityFacts {
    fn load(
        conn: &rusqlite::Connection,
        entity: &clarion_storage::EntityRow,
        project_root: &std::path::Path,
    ) -> clarion_storage::Result<Self> {
        let rel_path = entity.source_file_path.as_ref().map(|path| {
            std::path::Path::new(path)
                .strip_prefix(project_root)
                .ok()
                .and_then(|rel| rel.to_str())
                .unwrap_or(path)
                .to_owned()
        });

        let mut tags = HashSet::new();
        let mut stmt = conn.prepare("SELECT tag FROM entity_tags WHERE entity_id = ?1")?;
        let mut rows = stmt.query(rusqlite::params![entity.id])?;
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
}

/// guidance sheet ids that explicitly `guides` the given entity.
fn guides_edge_sources(
    conn: &rusqlite::Connection,
    entity_id: &str,
) -> clarion_storage::Result<HashSet<String>> {
    let mut set = HashSet::new();
    let mut stmt =
        conn.prepare("SELECT from_id FROM edges WHERE kind = 'guides' AND to_id = ?1")?;
    let mut rows = stmt.query(rusqlite::params![entity_id])?;
    while let Some(row) = rows.next()? {
        set.insert(row.get::<_, String>(0)?);
    }
    Ok(set)
}

/// A guidance sheet row read from `entities`.
#[derive(Clone)]
struct GuidanceRow {
    id: String,
    name: String,
    scope_level: Option<String>,
    scope_rank: Option<i64>,
    match_rules: Option<Vec<Value>>,
    content: Option<String>,
    expires: Option<String>,
    pinned: Option<bool>,
    provenance: Option<String>,
    authored_at: Option<String>,
}

impl GuidanceRow {
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        let match_rules = row
            .get::<_, Option<String>>(4)?
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|value| value.as_array().cloned());
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            scope_level: row.get(2)?,
            scope_rank: row.get(3)?,
            match_rules,
            content: row.get(5)?,
            expires: row.get(6)?,
            // `pinned` may be stored as a JSON bool; json_extract returns 0/1.
            pinned: row.get::<_, Option<i64>>(7)?.map(|v| v != 0),
            provenance: row.get(8)?,
            authored_at: row.get(9)?,
        })
    }
}

/// A composed (applicable) guidance sheet plus why it matched and its SEI.
#[derive(Clone)]
struct ComposedSheet {
    sheet: GuidanceRow,
    matched_by: Vec<String>,
    sei: Option<String>,
}

impl ComposedSheet {
    fn to_json(&self) -> Value {
        json!({
            "id": self.sheet.id,
            "sei": self.sei,
            "name": self.sheet.name,
            "scope_level": self.sheet.scope_level,
            "scope_rank": self.sheet.scope_rank,
            "content": self.sheet.content,
            "pinned": self.sheet.pinned,
            "provenance": self.sheet.provenance,
            "expires": self.sheet.expires,
            "matched_by": self.matched_by,
        })
    }
}

/// The verdict of evaluating one guidance match-rule against an entity.
enum RuleVerdict {
    Matched(&'static str),
    NoMatch,
    /// The rule cannot be evaluated at this surface (e.g. `wardline_group`,
    /// which would require parsing the opaque Wardline blob).
    Unevaluable,
}

fn rule_match(rule: &Value, facts: &EntityFacts) -> RuleVerdict {
    let Some(rule_type) = rule.get("type").and_then(Value::as_str) else {
        return RuleVerdict::NoMatch;
    };
    match rule_type {
        "path" => match (
            rule.get("pattern").and_then(Value::as_str),
            facts.rel_path.as_deref(),
        ) {
            (Some(pattern), Some(path)) if super::glob_match(pattern, path) => {
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

/// Optional `findings_for` filter (`kind` / `severity` / `status`).
struct FindingFilter {
    kind: Option<String>,
    severity: Option<String>,
    status: Option<String>,
}

impl FindingFilter {
    fn parse(arguments: &serde_json::Map<String, Value>) -> std::result::Result<Self, ParamError> {
        let Some(filter) = arguments.get("filter") else {
            return Ok(Self {
                kind: None,
                severity: None,
                status: None,
            });
        };
        let Some(object) = filter.as_object() else {
            return Err(ParamError::new("filter must be an object"));
        };
        let field = |name: &str| -> std::result::Result<Option<String>, ParamError> {
            match object.get(name) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(value)) => Ok(Some(value.clone())),
                Some(_) => Err(ParamError::new(&format!("filter.{name} must be a string"))),
            }
        };
        Ok(Self {
            kind: field("kind")?,
            severity: field("severity")?,
            status: field("status")?,
        })
    }

    fn matches(&self, finding: &FindingRow) -> bool {
        self.kind.as_ref().is_none_or(|k| *k == finding.kind)
            && self
                .severity
                .as_ref()
                .is_none_or(|s| *s == finding.severity)
            && self.status.as_ref().is_none_or(|s| *s == finding.status)
    }

    fn to_json(&self) -> Value {
        json!({
            "kind": self.kind,
            "severity": self.severity,
            "status": self.status,
        })
    }
}

/// A finding row anchored to the queried entity.
#[derive(Clone)]
struct FindingRow {
    id: String,
    tool: Option<String>,
    rule_id: Option<String>,
    kind: String,
    severity: String,
    status: String,
    message: Option<String>,
    related_entities: Option<String>,
    confidence: Option<f64>,
    created_at: Option<String>,
}

impl FindingRow {
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            tool: row.get(1)?,
            rule_id: row.get(2)?,
            kind: row.get(3)?,
            severity: row.get(4)?,
            status: row.get(5)?,
            message: row.get(6)?,
            related_entities: row.get(7)?,
            confidence: row.get(8)?,
            created_at: row.get(9)?,
        })
    }

    fn to_json(&self) -> Value {
        let related = self
            .related_entities
            .as_ref()
            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            .unwrap_or(Value::Array(Vec::new()));
        json!({
            "id": self.id,
            "tool": self.tool,
            "rule_id": self.rule_id,
            "kind": self.kind,
            "severity": self.severity,
            "status": self.status,
            "message": self.message,
            "related_entities": related,
            "confidence": self.confidence,
            "created_at": self.created_at,
        })
    }
}
