//! The `orientation_pack` consult-mode entrypoint.
//!
//! Extracted from `lib.rs` (V11-ARCH-04). Methods attach to
//! [`crate::ServerState`] via an inherent `impl` block; `lib.rs` keeps the
//! shared free-function helpers, the tool catalogue, and the JSON-RPC dispatch.

use loomweave_core::{EdgeConfidence, McpErrorCode};
use serde_json::{Value, json};

use loomweave_storage::{
    ReferenceDirection, StorageError, ancestor_chain, call_edges_from, call_edges_targeting,
    child_entity_ids, entities_containing_line, entity_by_id, has_any_alive_binding,
    normalize_source_path,
};

use crate::{
    ORIENTATION_PACK_MAX_NEIGHBORS, ORIENTATION_PACK_PATH_DEPTH, OrientationCore, ParamError,
    PathTraversal, ServerState, call_graph_scope_excludes, callee_json, caller_json,
    cap_neighbor_list, compact_execution_paths, entity_context_json, entity_json, import_neighbors,
    orientation_suggested_reads, path_truncation_reason, reference_neighbors_for, required_i64,
    storage_retryable, success_envelope, success_envelope_with_truncation, tool_error_envelope,
};

impl ServerState {
    #[allow(clippy::too_many_lines, clippy::similar_names)]
    pub(crate) async fn tool_orientation_pack(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        // Exactly one resolution form: an `entity` id, or a `file` + `line`.
        let entity_arg = arguments
            .get("entity")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let file_arg = arguments
            .get("file")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let has_line = arguments.get("line").is_some();

        // `query_line == Some` selects the file/line form; `None` the entity form.
        let (query_line, normalized_path, entity_id_arg) = match (entity_arg, file_arg, has_line) {
            (Some(id), None, false) => (None, None, Some(id.to_owned())),
            (None, Some(file), true) => {
                let line = required_i64(arguments, "line")?;
                if line <= 0 {
                    return Err(ParamError::new("line must be a positive integer"));
                }
                match normalize_source_path(&self.project_root, file) {
                    Ok(path) => (Some(line), Some(path), None),
                    Err(err) => {
                        return Ok(tool_error_envelope(
                            McpErrorCode::InvalidPath,
                            &err.to_string(),
                            false,
                        ));
                    }
                }
            }
            _ => {
                return Err(ParamError::new(
                    "provide exactly one of: `entity` (id), or `file` + `line`",
                ));
            }
        };

        let project_root = self.project_root.clone();
        let edge_cap = self.execution_edge_cap;
        let path_cap = self.execution_path_cap;

        let core = self
            .readers
            .with_reader(move |conn| {
                // Resolve the primary entity. The file/line form additionally
                // yields the containing candidate set for ambiguity reporting.
                let (matched, candidates) = if let Some(line) = query_line {
                    let path = normalized_path.as_deref().unwrap_or_default();
                    let candidates = entities_containing_line(conn, path, line)?;
                    (candidates.first().cloned(), candidates)
                } else {
                    let id = entity_id_arg.as_deref().unwrap_or_default();
                    match entity_by_id(conn, id)? {
                        Some(entity) => (Some(entity.clone()), vec![entity]),
                        None => (None, Vec::new()),
                    }
                };

                let snapshot = crate::snapshot::project_snapshot(conn, &project_root);
                let freshness = json!({
                    "staleness": snapshot.staleness(),
                    "last_analyzed_at": snapshot.last_analyzed_at(),
                    "degraded": snapshot.degraded(),
                    "scan_truncated": snapshot.scan_truncated(),
                });
                let staleness_stale = matches!(
                    snapshot.staleness(),
                    crate::snapshot::Staleness::Stale | crate::snapshot::Staleness::StaleWorktree
                );
                // Whether this index has any alive SEI bindings (REQ-C-04 /
                // ADR-038). Degrades to `false` on a pre-SEI database.
                let sei_populated = has_any_alive_binding(conn).unwrap_or(false);

                let Some(entity) = matched else {
                    return Ok(OrientationCore {
                        primary_id: None,
                        primary_kind: None,
                        lookup_was_id: query_line.is_none(),
                        packet: json!({
                            "primary_entity": Value::Null,
                            "entity_context":
                                entity_context_json(conn, query_line, None, &[], &[], &snapshot),
                            "source": Value::Null,
                            "neighbors": Value::Null,
                            "execution_paths": Value::Null,
                        }),
                        freshness,
                        staleness_stale,
                        sei_populated,
                        neighbors_omitted: serde_json::Map::new(),
                        paths_truncation_reason: None,
                        briefing_blocked: None,
                    });
                };

                // Refuse to build a pack for a briefing-blocked primary
                // (clarion-307668e2be): no identity, no surrounding structure —
                // mirroring the federation read API (ADR-034). Resolved here, in
                // the reader closure, so the post-closure path can short-circuit.
                if let Some(reason) = crate::briefing_block_reason(&entity) {
                    return Ok(OrientationCore {
                        primary_id: None,
                        primary_kind: None,
                        lookup_was_id: query_line.is_none(),
                        packet: Value::Null,
                        freshness,
                        staleness_stale,
                        sei_populated,
                        neighbors_omitted: serde_json::Map::new(),
                        paths_truncation_reason: None,
                        briefing_blocked: Some(reason),
                    });
                }

                let ancestors = ancestor_chain(conn, &entity.id)?;
                let entity_context = entity_context_json(
                    conn,
                    query_line,
                    Some(&entity),
                    &candidates,
                    &ancestors,
                    &snapshot,
                );

                let source = json!({
                    "source_file_path": entity.source_file_path,
                    "source_line_start": entity.source_line_start,
                    "source_line_end": entity.source_line_end,
                    "line_count": match (entity.source_line_start, entity.source_line_end) {
                        (Some(start), Some(end)) if end >= start => Some(end - start + 1),
                        _ => None,
                    },
                    "content_hash": entity.content_hash,
                });

                // One-hop neighbors at resolved confidence, each bounded.
                let confidence = EdgeConfidence::Resolved;
                let callers_all = call_edges_targeting(conn, &entity.id, confidence)?
                    .into_iter()
                    .filter_map(|edge| caller_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let callees_all = call_edges_from(conn, &entity.id, confidence)?
                    .into_iter()
                    .filter_map(|edge| callee_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let container = entity
                    .parent_id
                    .as_deref()
                    .and_then(|parent_id| entity_by_id(conn, parent_id).transpose())
                    .transpose()?
                    .as_ref()
                    .map(|e| entity_json(conn, e));
                let contained_all = child_entity_ids(conn, &entity.id)?
                    .iter()
                    .filter_map(|child_id| entity_by_id(conn, child_id).transpose())
                    .map(|row| row.map(|entity| entity_json(conn, &entity)))
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let (refs_in, references_rolled_up) =
                    reference_neighbors_for(conn, &entity, ReferenceDirection::In)?;
                let (refs_out, _) =
                    reference_neighbors_for(conn, &entity, ReferenceDirection::Out)?;
                let imports_in = import_neighbors(conn, &entity.id, ReferenceDirection::In)?;
                let imports_out = import_neighbors(conn, &entity.id, ReferenceDirection::Out)?;

                let cap = ORIENTATION_PACK_MAX_NEIGHBORS;
                let (callers, callers_omitted) = cap_neighbor_list(callers_all, cap);
                let (callees, callees_omitted) = cap_neighbor_list(callees_all, cap);
                let (contained, contained_omitted) = cap_neighbor_list(contained_all, cap);
                let (references_in, refs_in_omitted) = cap_neighbor_list(refs_in, cap);
                let (references_out, refs_out_omitted) = cap_neighbor_list(refs_out, cap);
                let (imports_in, imports_in_omitted) = cap_neighbor_list(imports_in, cap);
                let (imports_out, imports_out_omitted) = cap_neighbor_list(imports_out, cap);

                let scope_excludes = call_graph_scope_excludes(confidence);

                let neighbors = json!({
                    "callers": callers,
                    "callees": callees,
                    "container": container,
                    "contained": contained,
                    "references_in": references_in,
                    "references_out": references_out,
                    // See `tool_neighborhood`: module references_in/out are
                    // rolled up over contained symbols (clarion-79d0ff6e14).
                    "references_rolled_up": references_rolled_up,
                    "imports_in": imports_in,
                    "imports_out": imports_out,
                    "scope_excludes": scope_excludes,
                });

                // Compact resolved execution paths.
                let mut traversal = PathTraversal::new(edge_cap);
                let mut path = vec![entity.id.clone()];
                traversal.walk(
                    conn,
                    &entity.id,
                    &mut path,
                    ORIENTATION_PACK_PATH_DEPTH,
                    confidence,
                )?;
                let edge_truncated = traversal.truncated;
                let edge_count_visited = traversal.edge_count_visited;
                let compact = compact_execution_paths(conn, traversal.paths, path_cap)?;
                let paths_truncation_reason =
                    path_truncation_reason(edge_truncated, compact.path_cap_truncated);
                let execution_paths = json!({
                    "root": entity.id,
                    "nodes": compact.nodes,
                    "paths": compact.paths,
                    "edge_count_visited": edge_count_visited,
                    "truncated": paths_truncation_reason.is_some(),
                    "truncation_reason": paths_truncation_reason,
                });

                let mut neighbors_omitted = serde_json::Map::new();
                for (key, omitted) in [
                    ("callers", callers_omitted),
                    ("callees", callees_omitted),
                    ("contained", contained_omitted),
                    ("references_in", refs_in_omitted),
                    ("references_out", refs_out_omitted),
                    ("imports_in", imports_in_omitted),
                    ("imports_out", imports_out_omitted),
                ] {
                    neighbors_omitted.insert(key.to_owned(), json!(omitted));
                }

                Ok(OrientationCore {
                    primary_id: Some(entity.id.clone()),
                    primary_kind: Some(entity.kind.clone()),
                    lookup_was_id: query_line.is_none(),
                    packet: json!({
                        "primary_entity": entity_json(conn, &entity),
                        "entity_context": entity_context,
                        "source": source,
                        "neighbors": neighbors,
                        "execution_paths": execution_paths,
                    }),
                    freshness,
                    staleness_stale,
                    sei_populated,
                    neighbors_omitted,
                    paths_truncation_reason: paths_truncation_reason.map(str::to_owned),
                    briefing_blocked: None,
                })
            })
            .await;

        let core = match core {
            Ok(core) => core,
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };

        // A briefing-blocked primary is refused before any structure is built —
        // no identity, no neighbors, no paths (clarion-307668e2be). Checked ahead
        // of the not-found branch: the blocked core carries `primary_id: None`.
        if let Some(reason) = &core.briefing_blocked {
            return Ok(success_envelope(json!({
                "available": false,
                "briefing_blocked": reason,
                "remediation": crate::briefing_block_remediation(reason),
                "primary_entity": Value::Null,
            })));
        }

        // An `entity`-id lookup that resolved to nothing is a hard error; a
        // file/line lookup that spans nothing degrades to a no_match packet.
        if core.primary_id.is_none() && core.lookup_was_id {
            return Ok(tool_error_envelope(
                McpErrorCode::EntityNotFound,
                "no entity with the given id",
                false,
            ));
        }

        // Related Filigree issues — reuse `issues_for` so its disabled /
        // unreachable degradation paths are shared. Bounded to the primary
        // entity (no contained fan-out) to keep the packet small.
        let (issues, wardline_findings) = if let Some(primary_id) = &core.primary_id {
            let mut issue_args = serde_json::Map::new();
            issue_args.insert("id".to_owned(), json!(primary_id));
            issue_args.insert("include_contained".to_owned(), json!(false));
            match self.tool_issues_for(&issue_args).await {
                Ok(mut envelope) => {
                    // Flow B: lift the wardline_findings section out of the nested
                    // issues result so the pack surfaces it as a top-level section
                    // (issues_for nests it under `result`). Reuses the reconciliation
                    // issues_for already did for this same primary entity — no second
                    // Filigree fetch.
                    let wardline = envelope
                        .get_mut("result")
                        .and_then(Value::as_object_mut)
                        .and_then(|result| result.remove("wardline_findings"));
                    let issues = envelope.get("result").cloned().unwrap_or(Value::Null);
                    (issues, wardline)
                }
                Err(_) => (
                    json!({"available": false, "reason": "issues lookup failed"}),
                    None,
                ),
            }
        } else {
            (
                json!({"available": false, "reason": "no primary entity at this location"}),
                None,
            )
        };

        let health = json!({
            "index": core.freshness,
            // Whether this build understands SEIs (always true) and whether the
            // served index has SEI bindings populated (REQ-C-04 / ADR-038), so a
            // consult agent knows if entity `sei` fields in this pack are
            // non-null.
            "sei": {
                "supported": true,
                "populated": core.sei_populated,
            },
            "filigree": self.filigree_diagnostics_json(),
            "llm": self.llm_diagnostics_json(),
        });

        let neighbors_truncated = core
            .neighbors_omitted
            .values()
            .any(|value| value.as_u64().unwrap_or(0) > 0);
        let paths_truncated = core.paths_truncation_reason.is_some();

        let mut warnings: Vec<String> = Vec::new();
        if core.primary_id.is_none() {
            warnings.push(
                "No entity spans this location; only the enclosing scope (if any) is reported — \
                 not a guaranteed absence of code."
                    .to_owned(),
            );
        }
        if core.staleness_stale {
            warnings.push(
                "Index is stale: at least one ingested source file is newer than the last \
                 analyze run. Re-run `loomweave analyze`."
                    .to_owned(),
            );
        }
        if issues.get("available") == Some(&Value::Bool(false)) {
            let reason = issues
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unavailable");
            warnings.push(format!(
                "Filigree issues unavailable ({reason}); the related-issues section is empty for \
                 lack of data, not lack of issues."
            ));
        }
        if neighbors_truncated {
            warnings
                .push("Some neighbor lists were truncated; see `omitted` for counts.".to_owned());
        }
        if paths_truncated {
            warnings.push(
                "Execution paths were truncated; see `omitted.execution_paths_truncation_reason`."
                    .to_owned(),
            );
        }

        let suggested = orientation_suggested_reads(
            &core.packet,
            core.primary_id.as_deref(),
            core.primary_kind.as_deref(),
        );

        let mut omitted = core.neighbors_omitted.clone();
        omitted.insert(
            "execution_paths_truncated".to_owned(),
            json!(paths_truncated),
        );
        omitted.insert(
            "execution_paths_truncation_reason".to_owned(),
            json!(core.paths_truncation_reason),
        );

        let truncated = neighbors_truncated || paths_truncated;

        let mut packet = core.packet;
        let object = packet
            .as_object_mut()
            .expect("orientation packet is an object");
        object.insert("issues".to_owned(), issues);
        if let Some(wardline) = wardline_findings {
            object.insert("wardline_findings".to_owned(), wardline);
        }
        object.insert("health".to_owned(), health);
        object.insert("warnings".to_owned(), json!(warnings));
        object.insert("suggested_next_reads".to_owned(), json!(suggested));
        object.insert("omitted".to_owned(), Value::Object(omitted));

        Ok(success_envelope_with_truncation(
            packet,
            truncated.then_some("orientation-pack-bounds"),
        ))
    }
}
