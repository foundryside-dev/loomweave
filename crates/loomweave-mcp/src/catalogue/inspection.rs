//! WS5 inspection reads: `guidance_for`, `findings_for`, `wardline_for`.
//!
//! These are the read-side of the guidance / findings / wardline surfaces. They
//! READ composed/anchored records for one entity; authoring (WS6) is out of
//! scope. Each carries the queried entity through [`crate::entity_json`] so the
//! response is SEI-bearing (ADR-038).

use std::collections::HashSet;

use serde_json::{Value, json};

use loomweave_core::McpErrorCode;
use loomweave_storage::{
    MatchFacts, Resolution, RuleVerdict, entity_by_id, get_taint_facts, resolve_entity_ref,
    resolve_qualnames_all_kinds, rule_match, sei::is_reserved_sei, sei_for_locator,
};

use crate::ParamError;
use crate::ServerState;
use crate::catalogue::{Page, missing_signal, paginate};
use crate::{
    entity_json, flatten_storage_envelope_result, parse_to_unix_seconds, required_str,
    success_envelope, tool_error_envelope,
};

/// Bound on guidance sheets scanned per `guidance_for` call. Guidance is
/// authored, low-cardinality institutional knowledge; this only guards a
/// pathological project.
const GUIDANCE_SCAN_CAP: usize = 2000;

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
                let Some(entity) = resolve_entity_ref(conn, &entity_id)? else {
                    return Ok(tool_error_envelope(
                        McpErrorCode::EntityNotFound,
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                };

                let facts = MatchFacts::from_entity_row(conn, &entity, &project_root)?;
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

                    // Expiry: parse both `expires` and the server clock to Unix
                    // seconds (accepting `unix:<secs>` and RFC3339) and compare
                    // numerically. Skip only when both parse and the sheet's
                    // expiry precedes `now`. Fail open: a missing or unparseable
                    // `expires` (or an unparseable clock) never hides a sheet.
                    if let Some(exp) = sheet.expires.as_deref()
                        && let Some(exp_secs) = parse_to_unix_seconds(exp)
                        && let Some(now_secs) = parse_to_unix_seconds(&now)
                        && exp_secs < now_secs
                    {
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
                             Wardline blob is opaque to Loomweave; use wardline_for / find_by_wardline"
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
                let Some(entity) = resolve_entity_ref(conn, &entity_id)? else {
                    return Ok(tool_error_envelope(
                        McpErrorCode::EntityNotFound,
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                };

                let kind = filter.kind.as_deref();
                let severity = filter.severity.as_deref();
                let status = filter.status.as_deref();
                let total: usize = conn.query_row(
                    "SELECT COUNT(*) \
                       FROM findings \
                      WHERE entity_id = ?1 \
                        AND (?2 IS NULL OR kind = ?2) \
                        AND (?3 IS NULL OR severity = ?3) \
                        AND (?4 IS NULL OR status = ?4)",
                    rusqlite::params![entity.id, kind, severity, status],
                    |row| {
                        let count: i64 = row.get(0)?;
                        Ok(usize::try_from(count).unwrap_or(usize::MAX))
                    },
                )?;
                let mut stmt = conn.prepare(
                    "SELECT id, tool, rule_id, kind, severity, status, message, \
                            related_entities, confidence, created_at \
                       FROM findings \
                      WHERE entity_id = ?1 \
                        AND (?2 IS NULL OR kind = ?2) \
                        AND (?3 IS NULL OR severity = ?3) \
                        AND (?4 IS NULL OR status = ?4) \
                      ORDER BY created_at DESC, id \
                      LIMIT ?5 OFFSET ?6",
                )?;
                let limit = i64::try_from(page.limit).unwrap_or(i64::MAX);
                let offset = i64::try_from(page.offset).unwrap_or(i64::MAX);
                let mut rows = stmt.query(rusqlite::params![
                    entity.id, kind, severity, status, limit, offset
                ])?;
                let mut page_rows: Vec<FindingRow> = Vec::new();
                while let Some(row) = rows.next()? {
                    page_rows.push(FindingRow::from_row(row)?);
                }

                let returned = page_rows.len();
                let findings: Vec<Value> = page_rows.iter().map(FindingRow::to_json).collect();
                let meta = json!({
                    "total": total,
                    "offset": page.offset,
                    "limit": page.limit,
                    "returned": returned,
                    "truncated": page.offset.saturating_add(returned) < total,
                });

                Ok(success_envelope(json!({
                    "entity": entity_json(conn, &entity),
                    "findings": findings,
                    "filter": filter.to_json(),
                    "page": meta,
                    "scan_truncated": false,
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    /// `project_finding_list(filter?)` — every finding across the WHOLE project,
    /// no entity id required, so an agent can go from `project_status`'s
    /// `findings: N` count straight to the N findings (L1). Each row carries its
    /// anchoring entity (id, `sei`, file, line) plus the finding's
    /// `tool/rule_id/kind/severity/status/message/confidence/created_at`. Optionally
    /// filtered by `filter.kind`/`severity`/`status` (same vocabulary as
    /// `findings_for`). Bounded (limit/offset, total/truncated). With no filter
    /// the page `total` reconciles with `project_status`'s finding count: it is
    /// computed from the bare `findings` table (byte-identical to that count's
    /// query), the entity join only enriches the returned rows. Honest-empty:
    /// a project with no findings returns an empty list, not an error. Stateless.
    pub(crate) async fn tool_project_findings(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let filter = FindingFilter::parse(arguments)?;
        let page = Page::parse(arguments, FINDINGS_PAGE_DEFAULT, FINDINGS_PAGE_MAX)?;
        let result = self
            .readers
            .with_reader(move |conn| {
                let kind = filter.kind.as_deref();
                let severity = filter.severity.as_deref();
                let status = filter.status.as_deref();

                // Reconciliation contract (L1 acceptance #2): count off the bare
                // `findings` table with the SAME predicate the snapshot's
                // `finding_count()` uses, so an unfiltered total equals
                // project_status's finding count. The entity join below only
                // enriches the page rows — it never drives the total.
                let total: usize = conn.query_row(
                    "SELECT COUNT(*) \
                       FROM findings \
                      WHERE (?1 IS NULL OR kind = ?1) \
                        AND (?2 IS NULL OR severity = ?2) \
                        AND (?3 IS NULL OR status = ?3)",
                    rusqlite::params![kind, severity, status],
                    |row| {
                        let count: i64 = row.get(0)?;
                        Ok(usize::try_from(count).unwrap_or(usize::MAX))
                    },
                )?;

                // Page rows, joined to the anchoring entity for file:line. The FK
                // (`findings.entity_id REFERENCES entities(id) ON DELETE CASCADE`)
                // guarantees every finding has a live anchor, so this inner join
                // never drops a counted row.
                let mut stmt = conn.prepare(
                    "SELECT f.id, f.tool, f.rule_id, f.kind, f.severity, f.status, \
                            f.message, f.confidence, f.created_at, \
                            f.entity_id, e.source_file_path, e.source_line_start \
                       FROM findings f \
                       JOIN entities e ON e.id = f.entity_id \
                      WHERE (?1 IS NULL OR f.kind = ?1) \
                        AND (?2 IS NULL OR f.severity = ?2) \
                        AND (?3 IS NULL OR f.status = ?3) \
                      ORDER BY f.created_at DESC, f.id \
                      LIMIT ?4 OFFSET ?5",
                )?;
                let limit = i64::try_from(page.limit).unwrap_or(i64::MAX);
                let offset = i64::try_from(page.offset).unwrap_or(i64::MAX);
                let mut rows =
                    stmt.query(rusqlite::params![kind, severity, status, limit, offset])?;
                let mut page_rows: Vec<ProjectFindingRow> = Vec::new();
                while let Some(row) = rows.next()? {
                    page_rows.push(ProjectFindingRow::from_row(row)?);
                }

                let returned = page_rows.len();
                // Resolve each anchor's SEI while a reader connection is in scope.
                let findings: Vec<Value> = page_rows
                    .iter()
                    .map(|row| {
                        let sei = sei_for_locator(conn, &row.entity_id).ok().flatten();
                        row.to_json(sei.as_deref())
                    })
                    .collect();
                let meta = json!({
                    "total": total,
                    "offset": page.offset,
                    "limit": page.limit,
                    "returned": returned,
                    "truncated": page.offset.saturating_add(returned) < total,
                });

                Ok(success_envelope(json!({
                    "findings": findings,
                    "filter": filter.to_json(),
                    "page": meta,
                    "scan_truncated": false,
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    /// `wardline_for(entity_id)` — the Wardline metadata recorded for the entity
    /// (declared tier, groups, boundary contracts), returned **verbatim**: the
    /// `wardline_json` blob is opaque to Loomweave (federation opacity contract).
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
                let Some(entity) = resolve_entity_ref(conn, &entity_id)? else {
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
                        // failing — Loomweave never depends on its shape.
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

    /// `entity_resolve` — batch-resolve pasted identifiers (dotted qualnames,
    /// Rust `::` paths, SEI tokens) to entity ids + SEIs, the inverse of the
    /// id-taking tools (clarion-c2bb394f46).
    ///
    /// Qualname entries resolve through the all-kinds exact-tier resolver
    /// ([`resolve_qualnames_all_kinds`]) — every qualname-dialect entity kind
    /// participates (function, class, module, struct, trait, …; files and
    /// subsystems are not the qualname dialect and never resolve). This is
    /// deliberately a SEPARATE storage surface from the function-only ADR-036
    /// federation resolver, whose behavior is a cross-product contract
    /// (clarion-7b0795f9e8). `kind` and `plugin` are optional hard constraints
    /// with the ADR-036 hint semantics: unknown values match nothing (honest
    /// `unresolved`), never error. A pasted Rust `::` path normalizes to the
    /// stored dotted dialect (ADR-049) for resolution only — the result echoes
    /// the input as given.
    ///
    /// An entry in the reserved SEI namespace (`loomweave:eid:…`) is instead an
    /// exact identity lookup via [`resolve_entity_ref`]; constraints do not
    /// apply (an SEI is already exact — constraining it could only manufacture
    /// a false miss).
    ///
    /// Every hit projects through [`crate::entity_json`] so a candidate carries
    /// its SEI (ADR-038, stable entity identity) and a secret-scan-blocked
    /// entity collapses to the blocked stub — its id/sei are withheld exactly
    /// as the federation read API withholds them (ADR-034). Routing through
    /// `entity_json` (rather than hand-rolling the id+sei tuple) is
    /// load-bearing: it stops the reverse-map from becoming a side channel that
    /// discloses a blocked locator.
    ///
    /// Output is multi-candidate-shaped (`result_kind` + `candidates` list).
    /// A qualname existing under more than one (plugin, kind) yields
    /// `result_kind: "ambiguous"` with every candidate listed; the reserved
    /// `Resolution::Heuristic` variant slots in later without a schema break.
    pub(crate) async fn tool_entity_resolve(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        // Batch cap mirrors the federation `/api/wardline/resolve` surface
        // (`WARDLINE_TAINT_BATCH_MAX = 2000`); kept local to avoid a dependency
        // edge from loomweave-mcp onto loomweave-cli.
        const ENTITY_RESOLVE_BATCH_MAX: usize = 2000;

        // `identifiers` is a pure synonym for `qualnames` (clarion-057ff2b330):
        // a sibling tool pasting "identifiers" (its own vocabulary) hits the
        // same resolution pipeline. `qualnames` wins if both are present, for
        // backward compatibility.
        let Some(raw) = arguments
            .get("qualnames")
            .or_else(|| arguments.get("identifiers"))
            .and_then(Value::as_array)
        else {
            return Err(ParamError::new("qualnames must be a non-empty array"));
        };
        if raw.is_empty() {
            return Err(ParamError::new("qualnames must be a non-empty array"));
        }
        if raw.len() > ENTITY_RESOLVE_BATCH_MAX {
            return Err(ParamError::new(
                "qualnames exceeds the 2000-entry batch cap",
            ));
        }
        let mut qualnames = Vec::with_capacity(raw.len());
        for item in raw {
            let Some(qualname) = item.as_str() else {
                return Err(ParamError::new("each qualname must be a string"));
            };
            if qualname.trim().is_empty() {
                return Err(ParamError::new("qualnames must not contain a blank entry"));
            }
            qualnames.push(qualname.to_owned());
        }
        let kind = optional_constraint(arguments, "kind")?;
        let plugin = optional_constraint(arguments, "plugin")?;

        let result = self
            .readers
            .with_reader(move |conn| {
                // Qualname entries batch through the all-kinds resolver; SEI
                // entries are individual exact lookups. `::` → `.` applies to
                // the RESOLUTION input only; echoes keep the pasted form.
                let plain: Vec<String> = qualnames
                    .iter()
                    .filter(|entry| !is_reserved_sei(entry.trim()))
                    .map(|entry| entry.replace("::", "."))
                    .collect();
                let resolved =
                    resolve_qualnames_all_kinds(conn, &plain, kind.as_deref(), plugin.as_deref())?;
                let mut plain_results = resolved.into_iter();
                let mut results = Vec::with_capacity(qualnames.len());
                for entry in &qualnames {
                    let candidate_ids = if is_reserved_sei(entry.trim()) {
                        resolve_entity_ref(conn, entry.trim())?
                            .map_or_else(Vec::new, |row| vec![row.id])
                    } else {
                        let (_, resolution) = plain_results
                            .next()
                            .expect("one resolution per non-SEI entry");
                        match resolution {
                            Resolution::Exact { entity_id } => vec![entity_id],
                            Resolution::Ambiguous { entity_ids } => entity_ids,
                            Resolution::None => Vec::new(),
                        }
                    };
                    // Project EACH candidate through entity_json — the SEI
                    // attach + briefing-blocked stub collapse is a
                    // non-disclosure property that must apply to every
                    // candidate — then recompute result_kind from the count
                    // that SURVIVES vanished-row (torn-read) filtering:
                    // 0 → unresolved, 1 → resolved, >1 → ambiguous.
                    let mut candidates = Vec::with_capacity(candidate_ids.len());
                    for entity_id in candidate_ids {
                        // A candidate id resolved but its row vanished (a torn
                        // read): drop it, never error.
                        if let Some(entity) = entity_by_id(conn, &entity_id)? {
                            candidates.push(entity_json(conn, &entity));
                        }
                    }
                    let result_kind = match candidates.len() {
                        0 => "unresolved",
                        1 => "resolved",
                        _ => "ambiguous",
                    };
                    results.push(json!({
                        "qualname": entry,
                        "candidates": candidates,
                        "result_kind": result_kind,
                    }));
                }
                Ok(success_envelope(json!({
                    "results": results,
                    "scope_excludes": ["heuristic-tier-not-implemented"],
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }
}

/// Optional free-form constraint param (`kind` / `plugin` on
/// `entity_resolve`). Constraint semantics follow the ADR-036 plugin hint:
/// the value is NOT validated against the store (an unknown value is a
/// constraint nothing satisfies, resolving honest-`unresolved`), but a blank
/// value is a caller bug and rejected — mirroring the HTTP layer's
/// blank-rejection adjudication.
fn optional_constraint(
    arguments: &serde_json::Map<String, Value>,
    name: &str,
) -> std::result::Result<Option<String>, ParamError> {
    match arguments.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(Some(value.clone())),
        Some(_) => Err(ParamError::new(&format!(
            "{name} must be a non-blank string"
        ))),
    }
}

/// guidance sheet ids that explicitly `guides` the given entity.
fn guides_edge_sources(
    conn: &rusqlite::Connection,
    entity_id: &str,
) -> loomweave_storage::Result<HashSet<String>> {
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

/// Closed finding vocabularies (ADR-031 core-owned value sets; enforced as
/// CHECK constraints on the `findings` table in migration 0001). A filter
/// value outside its set can never match a row, so it is rejected up front —
/// the unknown-argument-KEY precedent extended to values (clarion-c137d73ebf).
const FINDING_KINDS: [&str; 5] = ["defect", "fact", "classification", "metric", "suggestion"];
const FINDING_SEVERITIES: [&str; 5] = ["INFO", "WARN", "ERROR", "CRITICAL", "NONE"];
const FINDING_STATUSES: [&str; 4] = ["open", "acknowledged", "suppressed", "promoted_to_issue"];

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
        let field =
            |name: &str, vocabulary: &[&str]| -> std::result::Result<Option<String>, ParamError> {
                match object.get(name) {
                    None | Some(Value::Null) => Ok(None),
                    Some(Value::String(value)) => {
                        // Canonical casing is mixed (severity upper, kind/status
                        // lower) and callers reliably type the other one, so
                        // matching is case-insensitive; the canonical spelling is
                        // what reaches the SQL predicate and the echoed filter.
                        match vocabulary.iter().find(|v| v.eq_ignore_ascii_case(value)) {
                            Some(canonical) => Ok(Some((*canonical).to_owned())),
                            None => Err(ParamError::new(&format!(
                                "filter.{name} must be one of {} (got \"{value}\")",
                                vocabulary.join(" | ")
                            ))),
                        }
                    }
                    Some(_) => Err(ParamError::new(&format!("filter.{name} must be a string"))),
                }
            };
        Ok(Self {
            kind: field("kind", &FINDING_KINDS)?,
            severity: field("severity", &FINDING_SEVERITIES)?,
            status: field("status", &FINDING_STATUSES)?,
        })
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

/// A finding row for the project-wide list, carrying its anchoring entity's
/// locator + `file:line` (the SEI is resolved at render time). No `related_entities`
/// — the project list answers "where are the N findings", and each row's primary
/// anchor is the entity it hangs on.
#[derive(Clone)]
struct ProjectFindingRow {
    id: String,
    tool: Option<String>,
    rule_id: Option<String>,
    kind: String,
    severity: String,
    status: String,
    message: Option<String>,
    confidence: Option<f64>,
    created_at: Option<String>,
    entity_id: String,
    entity_file: Option<String>,
    entity_line: Option<i64>,
}

impl ProjectFindingRow {
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            tool: row.get(1)?,
            rule_id: row.get(2)?,
            kind: row.get(3)?,
            severity: row.get(4)?,
            status: row.get(5)?,
            message: row.get(6)?,
            confidence: row.get(7)?,
            created_at: row.get(8)?,
            entity_id: row.get(9)?,
            entity_file: row.get(10)?,
            entity_line: row.get(11)?,
        })
    }

    fn to_json(&self, sei: Option<&str>) -> Value {
        json!({
            "id": self.id,
            "tool": self.tool,
            "rule_id": self.rule_id,
            "kind": self.kind,
            "severity": self.severity,
            "status": self.status,
            "message": self.message,
            "confidence": self.confidence,
            "created_at": self.created_at,
            "entity": {
                "id": self.entity_id,
                "sei": sei,
                "file": self.entity_file,
                "line": self.entity_line,
            },
        })
    }
}
