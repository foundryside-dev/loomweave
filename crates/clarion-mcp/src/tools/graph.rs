//! Graph & structure reads: `entity_at`, `find_entity`, `callers_of`,
//! `execution_paths_from`, `neighborhood`, `issues_for`, `subsystem_members`,
//! `subsystem_of`, `call_sites`.
//!
//! Extracted from `lib.rs` (V11-ARCH-04). Methods attach to
//! [`crate::ServerState`] via an inherent `impl` block; `lib.rs` keeps the
//! shared free-function helpers, the tool catalogue, and the JSON-RPC dispatch.

use std::collections::{BTreeSet, HashMap};

use clarion_core::{EdgeConfidence, McpErrorCode};
use serde_json::{Value, json};

use clarion_storage::{
    ReferenceDirection, StorageError, ancestor_chain, call_edges_from, call_edges_targeting,
    child_entity_ids, entities_containing_line, entity_by_id, find_entities, normalize_source_path,
    subsystem_members, subsystem_of_entity,
};

use crate::filigree::IssueDetail;

use crate::{
    CallSiteKind, CallSiteRole, InferredDispatchStats, IssuesForAccumulator, ParamError, PathScope,
    PathTraversal, ServerState, build_call_sites, call_graph_scope_excludes, callee_json,
    caller_json, compact_execution_paths, entity_context_json, entity_json, entity_properties_json,
    envelope_from_storage_result, flatten_storage_envelope_result, import_neighbors,
    issues_unavailable, optional_bool, optional_confidence, optional_usize, path_truncation_reason,
    reference_neighbors_for, required_i64, required_str, storage_retryable, success_envelope,
    success_envelope_with_stats, success_envelope_with_truncation,
    success_envelope_with_truncation_and_stats, tool_error_envelope, wardline_section_for_entity,
    wardline_unavailable,
};

impl ServerState {
    pub(crate) async fn tool_entity_at(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let file = required_str(arguments, "file")?.to_owned();
        let line = required_i64(arguments, "line")?;
        if line <= 0 {
            return Err(ParamError::new("line must be positive"));
        }
        let normalized = match normalize_source_path(&self.project_root, &file) {
            Ok(path) => path,
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::InvalidPath,
                    &err.to_string(),
                    false,
                ));
            }
        };
        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                // Every entity whose span contains the line, innermost first
                // (same ordering as the legacy single-row `entity_at_line`).
                let candidates = entities_containing_line(conn, &normalized, line)?;
                let matched = candidates.first().cloned();
                let stack = match &matched {
                    Some(entity) => ancestor_chain(conn, &entity.id)?,
                    None => Vec::new(),
                };
                let snapshot = crate::snapshot::project_snapshot(conn, &project_root);
                Ok(json!({
                    "entity": matched.as_ref().map(|e| entity_json(conn, e)),
                    "entity_context": entity_context_json(
                        conn,
                        Some(line),
                        matched.as_ref(),
                        &candidates,
                        &stack,
                        &snapshot,
                    ),
                }))
            })
            .await;
        Ok(envelope_from_storage_result(result))
    }

    pub(crate) async fn tool_find_entity(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let pattern = required_str(arguments, "pattern")?.to_owned();
        let limit = optional_usize(arguments, "limit")?
            .unwrap_or(20)
            .clamp(1, 100);
        let offset = match arguments.get("cursor") {
            None | Some(Value::Null) => 0,
            Some(Value::String(cursor)) => cursor
                .parse::<usize>()
                .map_err(|_| ParamError::new("cursor must be a numeric offset"))?,
            _ => return Err(ParamError::new("cursor must be a string or null")),
        };
        // Optional exact-match entity-kind filter (e.g. "subsystem"). Omitting it
        // preserves the unfiltered search. Validated as a non-blank string here;
        // unknown kinds simply match nothing (kinds are plugin-owned).
        let kind = match arguments.get("kind") {
            None | Some(Value::Null) => None,
            Some(Value::String(kind)) if !kind.trim().is_empty() => Some(kind.clone()),
            Some(Value::String(_)) => {
                return Err(ParamError::new("kind must be a non-empty string"));
            }
            _ => return Err(ParamError::new("kind must be a string or null")),
        };
        let result = self
            .readers
            .with_reader(move |conn| {
                let mut rows = find_entities(
                    conn,
                    &pattern,
                    limit.saturating_add(1),
                    offset,
                    kind.as_deref(),
                )?;
                let has_more = rows.len() > limit;
                rows.truncate(limit);
                let next_cursor = if has_more {
                    Some((offset + limit).to_string())
                } else {
                    None
                };
                Ok(json!({
                    "entities": rows.iter().map(|e| entity_json(conn, e)).collect::<Vec<_>>(),
                    "next_cursor": next_cursor
                }))
            })
            .await;
        Ok(envelope_from_storage_result(result))
    }

    pub(crate) async fn tool_callers_of(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let confidence = optional_confidence(arguments)?;
        let stats_delta = if confidence == EdgeConfidence::Inferred {
            match self.ensure_inferred_for_target(&entity_id).await {
                Ok(stats) => stats.to_json(),
                Err(err) => return Ok(err.to_envelope()),
            }
        } else {
            json!({})
        };
        let result = self
            .readers
            .with_reader(move |conn| {
                if entity_by_id(conn, &entity_id)?.is_none() {
                    return Ok(tool_error_envelope(
                        McpErrorCode::EntityNotFound,
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                }
                let callers = call_edges_targeting(conn, &entity_id, confidence)?
                    .into_iter()
                    .filter_map(|edge| caller_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                Ok(success_envelope_with_stats(
                    json!({
                        "callers": callers,
                        "scope_excludes": call_graph_scope_excludes(confidence),
                    }),
                    stats_delta,
                ))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    pub(crate) async fn tool_execution_paths_from(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let max_depth = optional_usize(arguments, "max_depth")?
            .unwrap_or(3)
            .clamp(1, 8);
        let confidence = optional_confidence(arguments)?;
        if confidence == EdgeConfidence::Inferred {
            return Ok(self.inferred_execution_paths(entity_id, max_depth).await);
        }
        let edge_cap = self.execution_edge_cap;
        let path_cap = self.execution_path_cap;
        let result = self
            .readers
            .with_reader(move |conn| {
                if entity_by_id(conn, &entity_id)?.is_none() {
                    return Ok(tool_error_envelope(
                        McpErrorCode::EntityNotFound,
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                }
                let mut traversal = PathTraversal::new(edge_cap);
                let mut path = vec![entity_id.clone()];
                traversal.walk(conn, &entity_id, &mut path, max_depth, confidence)?;
                let edge_truncated = traversal.truncated;
                let edge_count_visited = traversal.edge_count_visited;
                let compact = compact_execution_paths(conn, traversal.paths, path_cap)?;
                Ok(success_envelope_with_truncation(
                    json!({
                        "root": entity_id,
                        "nodes": compact.nodes,
                        "paths": compact.paths,
                        "edge_count_visited": edge_count_visited,
                        "scope_excludes": call_graph_scope_excludes(confidence),
                    }),
                    path_truncation_reason(edge_truncated, compact.path_cap_truncated),
                ))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    pub(crate) async fn inferred_execution_paths(
        &self,
        entity_id: String,
        max_depth: usize,
    ) -> Value {
        let exists = self
            .readers
            .with_reader({
                let entity_id = entity_id.clone();
                move |conn| entity_by_id(conn, &entity_id).map(|entity| entity.is_some())
            })
            .await;
        match exists {
            Ok(true) => {}
            Ok(false) => {
                return tool_error_envelope(
                    McpErrorCode::EntityNotFound,
                    &format!("entity {entity_id} was not found"),
                    false,
                );
            }
            Err(err) => {
                return tool_error_envelope(
                    McpErrorCode::StorageError,
                    &err.to_string(),
                    storage_retryable(&err),
                );
            }
        }

        let root = entity_id.clone();
        let mut stats = InferredDispatchStats::default();
        let mut dispatched_callers = BTreeSet::new();
        let mut stack = vec![(entity_id.clone(), vec![entity_id], max_depth)];
        let mut paths = Vec::new();
        let mut edge_count_visited = 0;
        let mut truncated = false;

        while let Some((current_id, path, remaining_depth)) = stack.pop() {
            if remaining_depth == 0 || truncated {
                continue;
            }
            if dispatched_callers.insert(current_id.clone()) {
                match self.ensure_inferred_for_caller(&current_id).await {
                    Ok(delta) => stats.merge(&delta),
                    Err(err) => return err.to_envelope(),
                }
            }
            let edges = match self
                .readers
                .with_reader({
                    let current_id = current_id.clone();
                    move |conn| call_edges_from(conn, &current_id, EdgeConfidence::Inferred)
                })
                .await
            {
                Ok(edges) => edges,
                Err(err) => {
                    return tool_error_envelope(
                        McpErrorCode::StorageError,
                        &err.to_string(),
                        storage_retryable(&err),
                    );
                }
            };
            for edge in edges.into_iter().rev() {
                edge_count_visited += 1;
                if edge_count_visited > self.execution_edge_cap {
                    truncated = true;
                    break;
                }
                if path.iter().any(|seen| seen == &edge.to_id) {
                    continue;
                }
                let mut next_path = path.clone();
                next_path.push(edge.to_id.clone());
                paths.push(next_path.clone());
                stack.push((edge.to_id, next_path, remaining_depth - 1));
            }
        }

        let path_cap = self.execution_path_cap;
        let compacted = self
            .readers
            .with_reader(move |conn| compact_execution_paths(conn, paths, path_cap))
            .await;
        match compacted {
            Ok(compact) => success_envelope_with_truncation_and_stats(
                json!({
                    "root": root,
                    "nodes": compact.nodes,
                    "paths": compact.paths,
                    "edge_count_visited": edge_count_visited,
                    "scope_excludes": call_graph_scope_excludes(EdgeConfidence::Inferred),
                }),
                path_truncation_reason(truncated, compact.path_cap_truncated),
                stats.to_json(),
            ),
            Err(err) => tool_error_envelope(
                McpErrorCode::StorageError,
                &err.to_string(),
                storage_retryable(&err),
            ),
        }
    }

    pub(crate) async fn tool_neighborhood(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let confidence = optional_confidence(arguments)?;
        if confidence == EdgeConfidence::Inferred {
            if let Err(err) = self.ensure_inferred_for_target(&entity_id).await {
                return Ok(err.to_envelope());
            }
            if let Err(err) = self.ensure_inferred_for_caller(&entity_id).await {
                return Ok(err.to_envelope());
            }
        }
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
                let inbound_callers = call_edges_targeting(conn, &entity_id, confidence)?
                    .into_iter()
                    .filter_map(|edge| caller_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let outbound_calls = call_edges_from(conn, &entity_id, confidence)?
                    .into_iter()
                    .filter_map(|edge| callee_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let container_entity = entity
                    .parent_id
                    .as_deref()
                    .and_then(|parent_id| entity_by_id(conn, parent_id).transpose())
                    .transpose()?
                    .as_ref()
                    .map(|e| entity_json(conn, e));
                let contained_entities = child_entity_ids(conn, &entity_id)?
                    .iter()
                    .filter_map(|child_id| entity_by_id(conn, child_id).transpose())
                    .map(|row| row.map(|entity| entity_json(conn, &entity)))
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let (references_in, references_rolled_up) =
                    reference_neighbors_for(conn, &entity, ReferenceDirection::In)?;
                let (references_out, _) =
                    reference_neighbors_for(conn, &entity, ReferenceDirection::Out)?;
                let imports_in = import_neighbors(conn, &entity_id, ReferenceDirection::In)?;
                let imports_out = import_neighbors(conn, &entity_id, ReferenceDirection::Out)?;
                let scope_excludes = call_graph_scope_excludes(confidence);
                Ok(success_envelope(json!({
                    "entity": entity_json(conn, &entity),
                    "callers": inbound_callers,
                    "callees": outbound_calls,
                    "container": container_entity,
                    "contained": contained_entities,
                    "references_in": references_in,
                    "references_out": references_out,
                    // True when the entity is a module and references_in/out
                    // aggregate contained symbols' edges (each neighbor tagged
                    // with a `via` symbol); false for symbol-level entities
                    // whose references are direct (clarion-79d0ff6e14).
                    "references_rolled_up": references_rolled_up,
                    "imports_in": imports_in,
                    "imports_out": imports_out,
                    "scope_excludes": scope_excludes,
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn tool_issues_for(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let include_contained = optional_bool(arguments, "include_contained")?.unwrap_or(true);
        // Surface the same configured-vs-resolved Filigree endpoint block that
        // `project_status` reports, so an agent can see WHICH endpoint a result
        // came from (e.g. an ethereal port resolved from
        // `.filigree/ephemeral.port`) instead of curling ports by hand. Null on
        // storage-only servers built without a diagnostics context.
        let endpoint = self.filigree_diagnostics_json();
        let Some(client) = self.filigree_client.clone() else {
            return Ok(issues_unavailable(
                &endpoint,
                "filigree-disabled",
                "Filigree integration is disabled",
            ));
        };
        // Capture the requested entity ID before it is moved into the storage
        // query for the wardline section lookup below.
        let requested_id = entity_id.clone();
        let read = match self
            .read_issues_for_entities(entity_id, include_contained)
            .await
        {
            Ok(Some(read)) => read,
            Ok(None) => {
                return Ok(issues_unavailable(
                    &endpoint,
                    "entity-not-found",
                    "Clarion entity was not found",
                ));
            }
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };
        let mut accumulator =
            IssuesForAccumulator::new(&read.entities, read.entity_json_by_id.clone());
        let mut requests_total = 0_usize;
        for (idx, entity) in read.entities.iter().enumerate() {
            let entity_id = entity.id.clone();
            let client = client.clone();
            let response = match tokio::task::spawn_blocking(move || {
                client.associations_for(&entity_id)
            })
            .await
            {
                Ok(Ok(response)) => response,
                Ok(Err(err)) => {
                    return Ok(issues_unavailable(
                        &endpoint,
                        "filigree-unreachable",
                        &err.to_string(),
                    ));
                }
                Err(err) => {
                    return Ok(issues_unavailable(
                        &endpoint,
                        "filigree-client-error",
                        &format!("Filigree client task failed: {err}"),
                    ));
                }
            };
            requests_total += 1;
            accumulator.add_response(response);
            if accumulator.emitted >= 100 && idx + 1 < read.entities.len() {
                accumulator.issue_cap_truncated = true;
                break;
            }
            if accumulator.issue_cap_truncated {
                break;
            }
        }
        // Enrich matched/drifted entries with each issue's title/status/priority.
        // Every unique issue is fetched exactly once (the accumulator already
        // dedupes issue_ids, so this is N requests for N distinct issues, never
        // per-entity N+1). Enrichment is best-effort and enrich-only: a 404
        // (issue/route absent) leaves that entry's `issue` null, and the first
        // transport/HTTP failure trips `route_down` so we stop hammering an
        // endpoint that has gone away rather than failing the whole call.
        let detail_ids = accumulator.enrichable_issue_ids();
        let mut details: HashMap<String, Option<IssueDetail>> = HashMap::new();
        let mut detail_requests_total = 0_usize;
        let mut route_down = false;
        for issue_id in detail_ids {
            if route_down {
                details.insert(issue_id, None);
                continue;
            }
            let client = client.clone();
            let id_for_task = issue_id.clone();
            let fetched =
                tokio::task::spawn_blocking(move || client.issue_detail(&id_for_task)).await;
            detail_requests_total += 1;
            match fetched {
                Ok(Ok(detail)) => {
                    details.insert(issue_id, detail);
                }
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, "clarion issues_for detail fetch failed; degrading to issue-id-only");
                    route_down = true;
                    details.insert(issue_id, None);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "clarion issues_for detail task failed; degrading to issue-id-only");
                    route_down = true;
                    details.insert(issue_id, None);
                }
            }
        }
        accumulator.apply_issue_details(&details);
        let mut envelope = accumulator.into_envelope(
            read.entity_cap_truncated,
            requests_total,
            detail_requests_total,
            &endpoint,
        );
        // Flow B: attach Wardline findings reconciled to the requested entity.
        if let Some(entity) = read.entities.iter().find(|e| e.id == requested_id) {
            let client = client.clone();
            let entity_id = entity.id.clone();
            let path = entity.source_file_path.clone();
            let section = tokio::task::spawn_blocking(move || {
                wardline_section_for_entity(&client, &entity_id, path.as_deref())
            })
            .await
            .unwrap_or_else(|err| wardline_unavailable(&format!("wardline task failed: {err}")));
            if let Some(result) = envelope.get_mut("result").and_then(Value::as_object_mut) {
                result.insert("wardline_findings".to_owned(), section);
            }
        }
        Ok(envelope)
    }

    pub(crate) async fn tool_subsystem_members(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let subsystem_id = required_str(arguments, "id")?.to_owned();
        let result = self
            .readers
            .with_reader(move |conn| {
                let Some(subsystem) = entity_by_id(conn, &subsystem_id)? else {
                    return Ok(tool_error_envelope(
                        McpErrorCode::EntityNotFound,
                        &format!("entity {subsystem_id} was not found"),
                        false,
                    ));
                };
                if subsystem.kind != "subsystem" {
                    return Ok(tool_error_envelope(
                        McpErrorCode::NotASubsystem,
                        &format!("entity {} is kind {}", subsystem.id, subsystem.kind),
                        false,
                    ));
                }
                let members = subsystem_members(conn, &subsystem.id)?
                    .iter()
                    .map(|member| {
                        json!({
                            "id": member.id,
                            "name": member.name,
                            "source_file_path": member.source_file_path
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(success_envelope(json!({
                    "subsystem": {
                        "id": subsystem.id,
                        "name": subsystem.name,
                        "short_name": subsystem.short_name,
                        "properties": entity_properties_json(&subsystem)
                    },
                    "members": members
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    pub(crate) async fn tool_subsystem_of(
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
                let Some(found) = subsystem_of_entity(conn, &entity.id)? else {
                    // Entity exists but has no subsystem-assigned module ancestor.
                    // A structural fact, not an error — return a success envelope
                    // with subsystem: null so an agent can distinguish it from a
                    // missing entity.
                    return Ok(success_envelope(json!({
                        "entity": {"id": entity.id, "kind": entity.kind},
                        "subsystem": Value::Null,
                        "via_module_id": Value::Null
                    })));
                };
                let subsystem = entity_by_id(conn, &found.subsystem_id)?;
                Ok(success_envelope(json!({
                    "entity": {"id": entity.id, "kind": entity.kind},
                    "subsystem": subsystem.as_ref().map(|s| json!({
                        "id": s.id,
                        "name": s.name,
                        "short_name": s.short_name,
                        "properties": entity_properties_json(s)
                    })),
                    "via_module_id": found.via_module_id
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    pub(crate) async fn tool_call_sites(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let role = match arguments.get("role") {
            None | Some(Value::Null) => CallSiteRole::Caller,
            Some(Value::String(s)) if s == "caller" => CallSiteRole::Caller,
            Some(Value::String(s)) if s == "callee" => CallSiteRole::Callee,
            _ => return Err(ParamError::new("role must be \"caller\" or \"callee\"")),
        };
        let kind = match arguments.get("kind") {
            None | Some(Value::Null) => CallSiteKind::Both,
            Some(Value::String(s)) if s == "calls" => CallSiteKind::Calls,
            Some(Value::String(s)) if s == "references" => CallSiteKind::References,
            _ => return Err(ParamError::new("kind must be \"calls\" or \"references\"")),
        };
        let path = match arguments.get("path") {
            None | Some(Value::Null) => PathScope::All,
            Some(Value::String(s)) if s == "all" => PathScope::All,
            Some(Value::String(s)) if s == "production" => PathScope::Production,
            Some(Value::String(s)) if s == "test" => PathScope::Test,
            _ => {
                return Err(ParamError::new(
                    "path must be \"all\", \"production\", or \"test\"",
                ));
            }
        };
        let confidence = optional_confidence(arguments)?;
        let result = self
            .readers
            .with_reader(move |conn| {
                build_call_sites(conn, &entity_id, role, kind, confidence, path)
            })
            .await;
        match result {
            Ok(Some(value)) => Ok(success_envelope(value)),
            Ok(None) => Ok(tool_error_envelope(
                McpErrorCode::NotFound,
                "no entity with the given id",
                false,
            )),
            Err(err) => Ok(tool_error_envelope(
                McpErrorCode::StorageError,
                &err.to_string(),
                storage_retryable(&err),
            )),
        }
    }
}
