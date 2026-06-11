//! WS5 faceted search: `find_by_tag`, `find_by_kind`, `find_by_wardline`.
//!
//! Each materialises a candidate set from a primary facet (capped), applies the
//! optional `scope` membership test and pagination in the read layer, and
//! returns SEI-bearing entity rows with the bounded-response metadata. Honest
//! empty: an unknown tag / kind, or a Wardline tier/group nothing carries,
//! yields an empty page (with `scan_truncated`/`scope_truncated` flags), never a
//! fabricated row.

use serde_json::{Value, json};

use loomweave_core::McpErrorCode;
use loomweave_storage::{
    EntityRow, entities_by_kind, entities_by_tag, entities_with_wardline_facts, get_taint_facts,
};

use crate::ParamError;
use crate::ServerState;
use crate::catalogue::{Page, RawScope, finalize_entity_page, missing_signal};
use crate::{entity_json, flatten_storage_envelope_result, required_str, success_envelope};

/// Candidate-set scan bound, shared with the entity-descendant scope cap so a
/// facet can never out-scan what scope can resolve.
const FACET_SCAN_CAP: usize = 50_000;
const FACET_PAGE_DEFAULT: usize = 50;
const FACET_PAGE_MAX: usize = 200;

impl ServerState {
    /// `find_by_tag(tag, scope?)` — entities carrying a plugin-emitted
    /// categorisation tag, scoped and paginated, SEI-carrying. Honest-empty when
    /// no entity carries the tag (no active plugin emits it).
    pub(crate) async fn tool_find_by_tag(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let tag = required_str(arguments, "tag")?.to_owned();
        let scope = RawScope::parse(arguments)?;
        let page = Page::parse(arguments, FACET_PAGE_DEFAULT, FACET_PAGE_MAX)?;
        Ok(self
            .tag_facet(
                tag,
                "no entity carries this categorisation tag; tags are populated by plugins \
                 (the Python plugin emits none today)",
                scope,
                page,
            )
            .await)
    }

    /// Shared core for tag-keyed facets: filter `entities_by_tag(tag)` by scope,
    /// paginate, render SEI-bearing rows, and surface a missing-signal note when
    /// the result is empty. Reused by `find_by_tag` and the categorisation
    /// shortcuts (`find_entry_points`, `find_tests`, …), each of which reads an
    /// existing tag and is honest-empty when no plugin emits it.
    pub(crate) async fn tag_facet(
        &self,
        tag: String,
        missing_reason: &'static str,
        scope: RawScope,
        page: Page,
    ) -> Value {
        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                let filter = scope.resolve(conn)?;
                let (candidates, scan_truncated) = entities_by_tag(conn, &tag, FACET_SCAN_CAP)?;
                let mut response = finalize_entity_page(
                    conn,
                    &project_root,
                    candidates,
                    &filter,
                    page,
                    scan_truncated,
                );
                attach_facet(&mut response, json!({ "tag": tag }));
                if response["page"]["total"] == json!(0) {
                    attach_signal(&mut response, missing_signal("entity_tags", missing_reason));
                }
                Ok(success_envelope(response))
            })
            .await;
        flatten_storage_envelope_result(result)
    }

    /// `find_by_kind(kind, scope?)` — entities of a plugin-declared kind, scoped
    /// and paginated, SEI-carrying.
    pub(crate) async fn tool_find_by_kind(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let kind = required_str(arguments, "kind")?.to_owned();
        let scope = RawScope::parse(arguments)?;
        let page = Page::parse(arguments, FACET_PAGE_DEFAULT, FACET_PAGE_MAX)?;
        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                let filter = scope.resolve(conn)?;
                let (candidates, scan_truncated) =
                    match entities_by_kind(conn, &kind, FACET_SCAN_CAP) {
                        Ok(found) => found,
                        Err(loomweave_storage::StorageError::InvalidQuery(message)) => {
                            return Ok(crate::tool_error_envelope(
                                McpErrorCode::InvalidPath,
                                &message,
                                false,
                            ));
                        }
                        Err(err) => return Err(err),
                    };
                // An unknown kind "matches no rows" by design (kinds are
                // plugin-owned, an open set), but a silent empty is
                // indistinguishable from "kind exists, nothing in scope" — so
                // when the kind matches zero entities project-wide, hint with
                // the kinds the index actually holds (clarion-c137d73ebf).
                let known_kinds = if candidates.is_empty() {
                    Some(loomweave_storage::known_entity_kinds(conn)?)
                } else {
                    None
                };
                let mut response = finalize_entity_page(
                    conn,
                    &project_root,
                    candidates,
                    &filter,
                    page,
                    scan_truncated,
                );
                attach_facet(&mut response, json!({ "kind": kind }));
                if let Some(kinds) = known_kinds
                    && let Some(object) = response.as_object_mut()
                {
                    object.insert("known_kinds".to_owned(), json!(kinds));
                }
                Ok(success_envelope(response))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    /// `find_by_wardline(tier?, group?, scope?)` — entities carrying a Wardline
    /// taint fact, optionally filtered by `tier`/`group`. The Wardline blob is
    /// opaque to Loomweave; tier/group filtering is **best-effort** (a top-level
    /// `tier`/`group` field on the blob) and honest-empty when the field is
    /// absent or no entity matches. Each returned entity carries its `wardline`
    /// blob verbatim plus its `sei`.
    pub(crate) async fn tool_find_by_wardline(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let tier = optional_facet(arguments, "tier")?;
        let group = optional_facet(arguments, "group")?;
        let has_findings = optional_bool(arguments, "has_findings")?;
        let scope = RawScope::parse(arguments)?;
        let page = Page::parse(arguments, FACET_PAGE_DEFAULT, FACET_PAGE_MAX)?;
        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                let filter = scope.resolve(conn)?;
                let (candidates, scan_truncated) =
                    entities_with_wardline_facts(conn, FACET_SCAN_CAP)?;

                // When `has_findings` is set, restrict to entities that carry at
                // least one finding — so an agent pages the fact-carrying-AND-flawed
                // entities, not every taint-fact blob (L1 complement). One bounded
                // query builds the set; absent the flag the filter is a no-op.
                let finding_anchor_ids: Option<std::collections::HashSet<String>> = if has_findings
                {
                    let mut set = std::collections::HashSet::new();
                    let mut stmt = conn.prepare("SELECT DISTINCT entity_id FROM findings")?;
                    let mut rows = stmt.query([])?;
                    while let Some(row) = rows.next()? {
                        set.insert(row.get::<_, String>(0)?);
                    }
                    Some(set)
                } else {
                    None
                };

                // Scope-filter first, then fetch the (opaque) blobs only for the
                // survivors — a narrow scope avoids reading every candidate blob.
                let in_scope: Vec<EntityRow> = candidates
                    .into_iter()
                    .filter(|e| {
                        filter.contains(&e.id, e.source_file_path.as_deref(), &project_root)
                            && finding_anchor_ids
                                .as_ref()
                                .is_none_or(|ids| ids.contains(&e.id))
                    })
                    .collect();
                let ids: Vec<String> = in_scope.iter().map(|e| e.id.clone()).collect();
                let facts = get_taint_facts(conn, &ids)?;
                let blobs: std::collections::HashMap<String, Value> = facts
                    .into_iter()
                    .map(|fact| {
                        let value = serde_json::from_str::<Value>(&fact.wardline_json)
                            .unwrap_or(Value::String(fact.wardline_json));
                        (fact.entity_id, value)
                    })
                    .collect();

                let matched: Vec<(EntityRow, Value)> = in_scope
                    .into_iter()
                    .filter_map(|e| {
                        let blob = blobs.get(&e.id).cloned().unwrap_or(Value::Null);
                        if facet_matches(&blob, "tier", tier.as_ref())
                            && facet_matches(&blob, "group", group.as_ref())
                        {
                            Some((e, blob))
                        } else {
                            None
                        }
                    })
                    .collect();

                let total = matched.len();
                let returned: Vec<(EntityRow, Value)> = matched
                    .into_iter()
                    .skip(page.offset)
                    .take(page.limit)
                    .collect();
                let returned_count = returned.len();
                let truncated = page.offset.saturating_add(returned_count) < total;
                let entities: Vec<Value> = returned
                    .iter()
                    .map(|(entity, blob)| {
                        let mut row = entity_json(conn, entity);
                        // The Wardline taint blob carries qualnames, which would
                        // survive the identity stub of a blocked entity — withhold
                        // it too (clarion-307668e2be).
                        let blob = if crate::briefing_block_reason(entity).is_some() {
                            Value::Null
                        } else {
                            blob.clone()
                        };
                        if let Some(object) = row.as_object_mut() {
                            object.insert("wardline".to_owned(), blob);
                        }
                        row
                    })
                    .collect();

                let mut response = json!({
                    "entities": entities,
                    "facet": { "tier": tier, "group": group, "has_findings": has_findings },
                    "page": {
                        "total": total,
                        "offset": page.offset,
                        "limit": page.limit,
                        "returned": returned_count,
                        "truncated": truncated,
                    },
                    "scope_truncated": filter.scope_truncated(),
                    "scan_truncated": scan_truncated,
                });
                if total == 0 {
                    attach_signal(
                        &mut response,
                        missing_signal(
                            "wardline_taint_facts",
                            "no entity matches; Wardline taint facts are populated via Filigree \
                             Flow-B (POST /api/wardline/taint-facts) and tier/group filtering is \
                             best-effort over the opaque blob",
                        ),
                    );
                }
                Ok(success_envelope(response))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }
}

/// Merge a `facet` echo block into a response object.
fn attach_facet(response: &mut Value, facet: Value) {
    if let Some(object) = response.as_object_mut() {
        object.insert("facet".to_owned(), facet);
    }
}

/// Attach a missing-signal note to a response object.
fn attach_signal(response: &mut Value, signal: Value) {
    if let Some(object) = response.as_object_mut() {
        object.insert("signal".to_owned(), signal);
    }
}

/// Parse an optional facet value (`tier`/`group`) accepting a string or a
/// number (numbers are stringified for the opaque-blob comparison).
fn optional_facet(
    arguments: &serde_json::Map<String, Value>,
    field: &str,
) -> std::result::Result<Option<String>, ParamError> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Number(value)) => Ok(Some(value.to_string())),
        Some(_) => Err(ParamError::new(&format!(
            "{field} must be a string or number"
        ))),
    }
}

/// Parse an optional boolean argument (`has_findings`). Absent / null → `false`.
fn optional_bool(
    arguments: &serde_json::Map<String, Value>,
    field: &str,
) -> std::result::Result<bool, ParamError> {
    match arguments.get(field) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ParamError::new(&format!("{field} must be a boolean"))),
    }
}

/// Best-effort match of a wanted facet value against a field on the opaque
/// Wardline blob. `None` wanted → always matches (no filter). Comparison is by
/// stringified value so `2` matches `"2"`.
fn facet_matches(blob: &Value, field: &str, wanted: Option<&String>) -> bool {
    let Some(wanted) = wanted else {
        return true;
    };
    match blob.get(field) {
        Some(Value::String(value)) => value == wanted,
        Some(Value::Number(value)) => value.to_string() == *wanted,
        Some(Value::Bool(value)) => value.to_string() == *wanted,
        _ => false,
    }
}
