//! WS5 faceted search: `find_by_tag`, `find_by_kind`, `find_by_wardline`.
//!
//! Each materialises a candidate set from a primary facet (capped), applies the
//! optional `scope` membership test and pagination in the read layer, and
//! returns SEI-bearing entity rows with the bounded-response metadata. Honest
//! empty: an unknown tag / kind, or a Wardline tier/group nothing carries,
//! yields an empty page (with `scan_truncated`/`scope_truncated` flags), never a
//! fabricated row.

use serde_json::{Value, json};

use clarion_core::McpErrorCode;
use clarion_storage::{
    EntityRow, entities_by_kind, entities_by_tag, entities_with_wardline_facts, get_taint_facts,
};

use crate::ParamError;
use crate::ServerState;
use crate::catalogue::{Page, RawScope, ScopeFilter, missing_signal};
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
        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                let filter = scope.resolve(conn)?;
                let (candidates, scan_truncated) = entities_by_tag(conn, &tag, FACET_SCAN_CAP)?;
                let mut response = finalize_entity_facet(
                    conn,
                    &project_root,
                    candidates,
                    &filter,
                    page,
                    scan_truncated,
                );
                attach_facet(&mut response, json!({ "tag": tag }));
                if response["page"]["total"] == json!(0) {
                    attach_signal(
                        &mut response,
                        missing_signal(
                            "entity_tags",
                            "no entity carries this categorisation tag; tags are populated by \
                             plugins (the Python plugin emits none today)",
                        ),
                    );
                }
                Ok(success_envelope(response))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
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
                let (candidates, scan_truncated) = match entities_by_kind(conn, &kind, FACET_SCAN_CAP)
                {
                    Ok(found) => found,
                    Err(clarion_storage::StorageError::InvalidQuery(message)) => {
                        return Ok(crate::tool_error_envelope(
                            McpErrorCode::InvalidPath,
                            &message,
                            false,
                        ));
                    }
                    Err(err) => return Err(err),
                };
                let mut response = finalize_entity_facet(
                    conn,
                    &project_root,
                    candidates,
                    &filter,
                    page,
                    scan_truncated,
                );
                attach_facet(&mut response, json!({ "kind": kind }));
                Ok(success_envelope(response))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    /// `find_by_wardline(tier?, group?, scope?)` — entities carrying a Wardline
    /// taint fact, optionally filtered by `tier`/`group`. The Wardline blob is
    /// opaque to Clarion; tier/group filtering is **best-effort** (a top-level
    /// `tier`/`group` field on the blob) and honest-empty when the field is
    /// absent or no entity matches. Each returned entity carries its `wardline`
    /// blob verbatim plus its `sei`.
    pub(crate) async fn tool_find_by_wardline(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let tier = optional_facet(arguments, "tier")?;
        let group = optional_facet(arguments, "group")?;
        let scope = RawScope::parse(arguments)?;
        let page = Page::parse(arguments, FACET_PAGE_DEFAULT, FACET_PAGE_MAX)?;
        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                let filter = scope.resolve(conn)?;
                let (candidates, scan_truncated) =
                    entities_with_wardline_facts(conn, FACET_SCAN_CAP)?;

                // Fetch the (opaque) blobs for best-effort tier/group filtering.
                let ids: Vec<String> = candidates.iter().map(|e| e.id.clone()).collect();
                let facts = get_taint_facts(conn, &ids)?;
                let blobs: std::collections::HashMap<String, Value> = facts
                    .into_iter()
                    .map(|fact| {
                        let value = serde_json::from_str::<Value>(&fact.wardline_json)
                            .unwrap_or(Value::String(fact.wardline_json));
                        (fact.entity_id, value)
                    })
                    .collect();

                let matched: Vec<(EntityRow, Value)> = candidates
                    .into_iter()
                    .filter(|e| filter.contains(&e.id, e.source_file_path.as_deref(), &project_root))
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
                        if let Some(object) = row.as_object_mut() {
                            object.insert("wardline".to_owned(), blob.clone());
                        }
                        row
                    })
                    .collect();

                let mut response = json!({
                    "entities": entities,
                    "facet": { "tier": tier, "group": group },
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

/// Filter `candidates` by `scope`, paginate, and render SEI-bearing entity rows
/// with bounded-response metadata. Consumes the candidate vec (no clone).
fn finalize_entity_facet(
    conn: &rusqlite::Connection,
    project_root: &std::path::Path,
    candidates: Vec<EntityRow>,
    scope: &ScopeFilter,
    page: Page,
    scan_truncated: bool,
) -> Value {
    let in_scope: Vec<EntityRow> = candidates
        .into_iter()
        .filter(|e| scope.contains(&e.id, e.source_file_path.as_deref(), project_root))
        .collect();
    let total = in_scope.len();
    let returned: Vec<EntityRow> = in_scope
        .into_iter()
        .skip(page.offset)
        .take(page.limit)
        .collect();
    let returned_count = returned.len();
    let truncated = page.offset.saturating_add(returned_count) < total;
    let entities: Vec<Value> = returned.iter().map(|e| entity_json(conn, e)).collect();
    json!({
        "entities": entities,
        "page": {
            "total": total,
            "offset": page.offset,
            "limit": page.limit,
            "returned": returned_count,
            "truncated": truncated,
        },
        "scope_truncated": scope.scope_truncated(),
        "scan_truncated": scan_truncated,
    })
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
