//! Graph & structure reads: `entity_at`, `find_entity`, `callers_of`,
//! `execution_paths_from`, `neighborhood`, `issues_for`, `subsystem_members`,
//! `subsystem_of`, `call_sites`.
//!
//! Extracted from `lib.rs` (V11-ARCH-04). Methods attach to
//! [`crate::ServerState`] via an inherent `impl` block; `lib.rs` keeps the
//! shared free-function helpers, the tool catalogue, and the JSON-RPC dispatch.

use std::collections::{BTreeSet, HashMap};

use loomweave_core::{EdgeConfidence, McpErrorCode};
use serde_json::{Value, json};

use loomweave_storage::{
    EntityVisibility, RELATION_EDGE_KINDS, ReferenceDirection, StorageError, ancestor_chain,
    call_edges_from, call_edges_targeting, child_entity_ids, entities_containing_line,
    entity_by_id, entity_visibility, find_entities, live_unresolved_call_sites_exist,
    normalize_source_path, relation_edges_for_entity, resolve_entity_ref, subsystem_members,
    subsystem_of_entity,
};

use crate::filigree::IssueDetail;

use crate::{
    CallSiteKind, CallSiteRole, InferredDispatchStats, IssuesForAccumulator, ParamError, PathScope,
    PathTraversal, ServerState, build_call_sites, build_unresolved_candidates,
    call_graph_scope_excludes, callee_json, caller_json, caller_navigation_scope_excludes,
    compact_execution_paths, entity_context_json, entity_json, entity_not_found_envelope,
    entity_properties_json, envelope_from_storage_result, flatten_storage_envelope_result,
    import_neighbors, issues_unavailable, navigation_scope_excludes, optional_bool,
    optional_confidence, optional_usize, parse_cursor_offset, path_truncation_reason,
    reference_neighbors_for, relation_neighbors, required_i64, required_str, storage_retryable,
    success_envelope, success_envelope_with_stats, success_envelope_with_truncation,
    success_envelope_with_truncation_and_stats, tool_error_envelope, unresolved_match_fields,
    wardline_section_for_entity, wardline_unavailable,
};

/// The direction argument of [`ServerState::tool_relation_list`]: a single
/// stored-edge direction, or `Both` (the default — clarion-057ff2b330) which
/// unions the In and Out passes. `Both` is the only value not directly
/// expressible as a [`ReferenceDirection`], so it needs this wrapper.
#[derive(Debug, Clone, Copy)]
enum RelationDirection {
    Single(ReferenceDirection),
    Both,
}

impl RelationDirection {
    /// The edge passes this direction expands to, in response order: a single
    /// direction is one pass; `Both` is In then Out.
    fn passes(self) -> &'static [ReferenceDirection] {
        match self {
            Self::Single(ReferenceDirection::In) => &[ReferenceDirection::In],
            Self::Single(ReferenceDirection::Out) => &[ReferenceDirection::Out],
            Self::Both => &[ReferenceDirection::In, ReferenceDirection::Out],
        }
    }

    /// The top-level `direction` echo for the response envelope.
    fn as_str(self) -> &'static str {
        match self {
            Self::Single(ReferenceDirection::In) => "in",
            Self::Single(ReferenceDirection::Out) => "out",
            Self::Both => "both",
        }
    }
}

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
        let requested_id = required_str(arguments, "id")?.to_owned();
        let confidence = optional_confidence(arguments)?;
        // Bounded single-relation shape (clarion-d76e7f7267): limit (default 50,
        // clamp 1..=100) + numeric-offset cursor; emit next_cursor + truncated.
        let limit = optional_usize(arguments, "limit")?
            .unwrap_or(50)
            .clamp(1, 100);
        let offset = parse_cursor_offset(arguments)?;
        // Canonicalize the id-or-SEI to its locator ONCE, before the
        // `ensure_inferred_*` pre-gate, so the inference pass and the reader both
        // key on the real `entities.id` (clarion-d76e7f7267).
        let entity_id = match self.resolve_to_locator(&requested_id).await {
            Ok(Some(id)) => id,
            Ok(None) => return Ok(entity_not_found_envelope(&requested_id)),
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };
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
                let mut callers = call_edges_targeting(conn, &entity_id, confidence)?
                    .into_iter()
                    .filter_map(|edge| caller_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                // Slice the materialised Vec at the MCP layer (the storage helper
                // takes no LIMIT/OFFSET). `offset` past the end yields an empty
                // page; `has_more` drives next_cursor + the explicit truncated.
                let total = callers.len();
                let page: Vec<_> = callers.drain(..).skip(offset).take(limit).collect();
                let has_more = offset.saturating_add(limit) < total;
                let next_cursor = has_more.then(|| (offset + limit).to_string());
                // Honesty fields (clarion-df87b4f381): name-matched unresolved
                // call sites are NOT in `callers`; say how many exist and where
                // to see them, and name the blind spot in scope_excludes.
                // Per-query honesty (clarion-76c31b730a): scope_excludes is now
                // populated ONLY when THIS traversal actually skipped a candidate
                // (a name-matched unresolved call site for this target), and the
                // skipped sites themselves are surfaced as `unresolved_candidates`
                // — the in-tool grep-fallback. An empty scope_excludes paired with
                // `traversal_complete: true` confirms every candidate was searched.
                let (unresolved_name_matches, next_action, unresolved_candidates) =
                    match entity_by_id(conn, &entity_id)? {
                        Some(target) => {
                            let (count, next_action) = unresolved_match_fields(conn, &target)?;
                            // Gate `unresolved_candidates` behind the same
                            // `confidence != Inferred` check that drives
                            // `caller_navigation_scope_excludes` (A1): the inferred
                            // dispatch attempts the unresolved category, so it skips
                            // nothing and scope_excludes is forced empty
                            // (traversal_complete:true). Surfacing candidates here
                            // anyway would contradict that completeness signal.
                            let candidates = if confidence == EdgeConfidence::Inferred {
                                Vec::new()
                            } else {
                                build_unresolved_candidates(conn, &target)?
                            };
                            (count, next_action, candidates)
                        }
                        None => (0, Value::Null, Vec::new()),
                    };
                let scope_excludes =
                    caller_navigation_scope_excludes(confidence, unresolved_name_matches > 0);
                let traversal_complete = scope_excludes.is_empty();
                Ok(success_envelope_with_stats(
                    json!({
                        "callers": page,
                        "next_cursor": next_cursor,
                        "truncated": has_more,
                        "unresolved_name_matches": unresolved_name_matches,
                        "next_action": next_action,
                        "scope_excludes": scope_excludes,
                        "traversal_complete": traversal_complete,
                        "unresolved_candidates": unresolved_candidates,
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
        let requested_id = required_str(arguments, "id")?.to_owned();
        let max_depth = optional_usize(arguments, "max_depth")?
            .unwrap_or(3)
            .clamp(1, 8);
        let confidence = optional_confidence(arguments)?;
        // Canonicalize id-or-SEI to its locator ONCE, before the inferred-branch
        // dispatch (which runs its own `ensure_inferred_*` pass) and the reader,
        // so all downstream traversal keys on the real id (clarion-d76e7f7267).
        let entity_id = match self.resolve_to_locator(&requested_id).await {
            Ok(Some(id)) => id,
            Ok(None) => return Ok(entity_not_found_envelope(&requested_id)),
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };
        if confidence == EdgeConfidence::Inferred {
            return Ok(self.inferred_execution_paths(entity_id, max_depth).await);
        }
        let edge_cap = self.execution_edge_cap;
        let path_cap = self.execution_path_cap;
        let result = self
            .readers
            .with_reader(move |conn| {
                let mut traversal = PathTraversal::new(edge_cap);
                let mut path = vec![entity_id.clone()];
                traversal.walk(conn, &entity_id, &mut path, max_depth, confidence)?;
                let edge_truncated = traversal.truncated;
                let edge_count_visited = traversal.edge_count_visited;
                let compact = compact_execution_paths(conn, traversal.paths, path_cap)?;
                let live_unresolved = live_unresolved_call_sites_exist(conn)?;
                Ok(success_envelope_with_truncation(
                    json!({
                        "root": entity_id,
                        "nodes": compact.nodes,
                        "paths": compact.paths,
                        "edge_count_visited": edge_count_visited,
                        "scope_excludes": navigation_scope_excludes(confidence, live_unresolved),
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
        let requested_id = required_str(arguments, "id")?.to_owned();
        let confidence = optional_confidence(arguments)?;
        // Per-bucket cap (clarion-d76e7f7267): ONE `limit` (default 50, clamp
        // 1..=100) bounds EACH of the nine list buckets independently. NO
        // cursor — one cursor cannot coherently advance nine heterogeneous
        // buckets; an agent paginates a specific relation via its dedicated tool.
        let limit = optional_usize(arguments, "limit")?
            .unwrap_or(50)
            .clamp(1, 100);
        // Canonicalize id-or-SEI to its locator ONCE, before both
        // `ensure_inferred_*` pre-gates and the reader (clarion-d76e7f7267).
        let entity_id = match self.resolve_to_locator(&requested_id).await {
            Ok(Some(id)) => id,
            Ok(None) => return Ok(entity_not_found_envelope(&requested_id)),
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };
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
                // Refuse to fan out structure around a briefing-blocked entity
                // (clarion-307668e2be) — withholding the surrounding graph is the
                // federation posture (ADR-034). A blocked entity that appears as a
                // *neighbor* of a visible entity is stubbed, not refused.
                if let Some(reason) = crate::briefing_block_reason(&entity) {
                    return Ok(crate::blocked_entity_refusal(&reason));
                }
                let mut inbound_callers = call_edges_targeting(conn, &entity_id, confidence)?
                    .into_iter()
                    .filter_map(|edge| caller_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let mut outbound_calls = call_edges_from(conn, &entity_id, confidence)?
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
                let mut contained_entities = child_entity_ids(conn, &entity_id)?
                    .iter()
                    .filter_map(|child_id| entity_by_id(conn, child_id).transpose())
                    .map(|row| row.map(|entity| entity_json(conn, &entity)))
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let (mut references_in, references_rolled_up) =
                    reference_neighbors_for(conn, &entity, ReferenceDirection::In)?;
                let (mut references_out, _) =
                    reference_neighbors_for(conn, &entity, ReferenceDirection::Out)?;
                let mut imports_in = import_neighbors(conn, &entity_id, ReferenceDirection::In)?;
                let mut imports_out = import_neighbors(conn, &entity_id, ReferenceDirection::Out)?;
                let mut relations_in =
                    relation_neighbors(conn, &entity_id, ReferenceDirection::In, confidence)?;
                let mut relations_out =
                    relation_neighbors(conn, &entity_id, ReferenceDirection::Out, confidence)?;
                // Honesty fields for the `callers` bucket (clarion-df87b4f381).
                let (unresolved_name_matches, next_action) =
                    unresolved_match_fields(conn, &entity)?;
                // Per-query honesty (clarion-76c31b730a): populate scope_excludes
                // ONLY when this traversal skipped a name-matched candidate, and
                // surface the skipped sites as `unresolved_candidates`. Empty
                // scope_excludes + `traversal_complete: true` confirms the callers
                // bucket searched every candidate.
                // Gate behind `confidence != Inferred` to mirror
                // `caller_navigation_scope_excludes` (A1): an inferred traversal
                // skips nothing, so it must not surface unresolved candidates
                // alongside `traversal_complete: true`.
                let unresolved_candidates = if confidence == EdgeConfidence::Inferred {
                    Vec::new()
                } else {
                    build_unresolved_candidates(conn, &entity)?
                };
                let scope_excludes =
                    caller_navigation_scope_excludes(confidence, unresolved_name_matches > 0);
                let traversal_complete = scope_excludes.is_empty();
                // Bound EACH bucket independently and record whether it was
                // trimmed in the sibling `truncated` map. A trimmed bucket directs
                // the agent to the dedicated single-relation tool for the full
                // cursor-paginated set (clarion-d76e7f7267).
                let truncate_bucket = |bucket: &mut Vec<Value>| -> bool {
                    let trimmed = bucket.len() > limit;
                    bucket.truncate(limit);
                    trimmed
                };
                let truncated = json!({
                    "callers": truncate_bucket(&mut inbound_callers),
                    "callees": truncate_bucket(&mut outbound_calls),
                    "contained": truncate_bucket(&mut contained_entities),
                    "references_in": truncate_bucket(&mut references_in),
                    "references_out": truncate_bucket(&mut references_out),
                    "imports_in": truncate_bucket(&mut imports_in),
                    "imports_out": truncate_bucket(&mut imports_out),
                    "relations_in": truncate_bucket(&mut relations_in),
                    "relations_out": truncate_bucket(&mut relations_out),
                });
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
                    // Kind-tagged relation edges (inherits_from / decorates /
                    // implements / derives, ADR-051). relations_in on a class
                    // answers "what subclasses this"; relations_out on a
                    // decorator answers "what does this decorate".
                    "relations_in": relations_in,
                    "relations_out": relations_out,
                    "truncated": truncated,
                    "unresolved_name_matches": unresolved_name_matches,
                    "next_action": next_action,
                    "scope_excludes": scope_excludes,
                    "traversal_complete": traversal_complete,
                    "unresolved_candidates": unresolved_candidates,
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
        // `.weft/filigree/ephemeral.port`) instead of curling ports by hand. Null on
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
                    "Loomweave entity was not found",
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
            let lookup_id = read
                .entity_json_by_id
                .get(&entity.id)
                .and_then(|json| json.get("sei"))
                .and_then(Value::as_str)
                .filter(|sei| !sei.trim().is_empty())
                .map_or_else(|| entity.id.clone(), ToOwned::to_owned);

            let client = client.clone();
            let response = match tokio::task::spawn_blocking(move || {
                client.associations_for(&lookup_id)
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
            // Stop if this response itself overflowed the issue cap, or if we've
            // hit the cap and there are still entities left to query.
            if accumulator.issue_cap_truncated {
                break;
            }
            if accumulator.emitted >= 100 && idx + 1 < read.entities.len() {
                accumulator.issue_cap_truncated = true;
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
        // `Some(reason)` once the first transport/HTTP failure trips the
        // degrade; carried into the envelope as an in-band marker
        // (weft-4a46553503 / dogfood-4 B9: enrichment 401'd and every issue
        // silently came back null — the consumer needs the WHY in-band).
        let mut detail_route_down: Option<String> = None;
        for issue_id in detail_ids {
            if detail_route_down.is_some() {
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
                    tracing::warn!(error = %err, "loomweave issues_for detail fetch failed; degrading to issue-id-only");
                    detail_route_down = Some(err.to_string());
                    details.insert(issue_id, None);
                }
                Err(err) => {
                    tracing::warn!(error = %err, "loomweave issues_for detail task failed; degrading to issue-id-only");
                    detail_route_down = Some(format!("detail task failed: {err}"));
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
        // Honest degrade (C-10): when the detail enrichment died mid-flight,
        // say so once, in-band — `issue: null` rows are otherwise
        // indistinguishable from "issue genuinely absent at the route".
        if let Some(reason) = detail_route_down
            && let Some(result) = envelope.get_mut("result").and_then(Value::as_object_mut)
        {
            result.insert(
                "issue_detail_unavailable".to_owned(),
                json!({
                    "reason": reason,
                    "note": "the issue-detail enrichment failed mid-call, so `issue` is null \
                             on the affected rows; `issue_id` remains authoritative — retry \
                             or query Filigree directly for title/status",
                }),
            );
        }
        // Flow B: attach Wardline findings reconciled to the requested entity.
        if let Some(entity) = read.entities.iter().find(|e| e.id == requested_id) {
            let client = client.clone();
            let entity_id = entity.id.clone();
            let path = entity.source_file_path.clone();
            let project_root = self.project_root.clone();
            let section = tokio::task::spawn_blocking(move || {
                wardline_section_for_entity(&client, &project_root, &entity_id, path.as_deref())
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
        // Bounded single-relation shape (clarion-d76e7f7267): a subsystem can
        // hold hundreds of modules. limit (default 50, clamp 1..=100) + cursor.
        let limit = optional_usize(arguments, "limit")?
            .unwrap_or(50)
            .clamp(1, 100);
        let offset = parse_cursor_offset(arguments)?;
        let result = self
            .readers
            .with_reader(move |conn| {
                let Some(subsystem) = resolve_entity_ref(conn, &subsystem_id)? else {
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
                // Slice the members Vec at the MCP layer (the storage helper takes
                // no LIMIT/OFFSET) before projecting, so paging bounds the work.
                let all_members = subsystem_members(conn, &subsystem.id)?;
                let total = all_members.len();
                let has_more = offset.saturating_add(limit) < total;
                let next_cursor = has_more.then(|| (offset + limit).to_string());
                // Members are projected with their own compact shape (not
                // `entity_json`). Under A3 (clarion-719e7320f5) a blocked member
                // module (its file carries a secret) keeps its navigable id/name/
                // path and rides the `briefing_blocked` flag — the secret is the
                // file content, not the structural identity. `entity_visibility`
                // supplies the block reason.
                let members = all_members
                    .iter()
                    .skip(offset)
                    .take(limit)
                    .map(|member| {
                        let briefing_blocked = match entity_visibility(conn, &member.id)? {
                            EntityVisibility::Blocked(reason) => Value::String(reason),
                            _ => Value::Null,
                        };
                        Ok(json!({
                            "id": member.id,
                            "name": member.name,
                            "source_file_path": member.source_file_path,
                            "briefing_blocked": briefing_blocked
                        }))
                    })
                    .collect::<Result<Vec<_>, StorageError>>()?;
                Ok(success_envelope(json!({
                    "subsystem": {
                        "id": subsystem.id,
                        "name": subsystem.name,
                        "short_name": subsystem.short_name,
                        "properties": entity_properties_json(&subsystem)
                    },
                    "members": members,
                    "next_cursor": next_cursor,
                    "truncated": has_more
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
                let Some(entity) = resolve_entity_ref(conn, &entity_id)? else {
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

    /// `entity_relation_list` (clarion-ae5b43ea40): the dedicated read surface
    /// for the relation edge kinds (`inherits_from` / `decorates` /
    /// `implements` / `derives`), previously write-only. The neighborhood /
    /// orientation `relations_in`/`relations_out` buckets serve the same edges
    /// kind-tagged; this tool is the cursor-paginated set WITH anchor evidence.
    ///
    /// Direction is positional over the stored edge; ADR-051 pins what that
    /// means per kind. The anchor evidence resolves through the edge's OWN
    /// `source_file_id` (never inferred from an endpoint): which side's file
    /// holds the anchor varies per kind — `decorates` anchors in the TO
    /// (decorated) side's file, every other kind in the FROM side's — so the
    /// edge's stored file is the only kind-agnostic truth.
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn tool_relation_list(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let requested_id = required_str(arguments, "id")?.to_owned();
        // Direction defaults to "both" (clarion-057ff2b330): an omitted direction
        // returns the in+out union rather than erroring, so an agent need not know
        // a kind's positional convention (ADR-051) to ask "every relation on X".
        // "in"/"out" keep their exact prior single-direction behavior.
        let direction = match arguments.get("direction") {
            None | Some(Value::Null) => RelationDirection::Both,
            Some(Value::String(s)) if s == "in" => {
                RelationDirection::Single(ReferenceDirection::In)
            }
            Some(Value::String(s)) if s == "out" => {
                RelationDirection::Single(ReferenceDirection::Out)
            }
            Some(Value::String(s)) if s == "both" => RelationDirection::Both,
            _ => {
                return Err(ParamError::new(
                    "direction must be \"in\", \"out\", or \"both\"",
                ));
            }
        };
        let kind = match arguments.get("kind") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if RELATION_EDGE_KINDS.contains(&s.as_str()) => Some(s.clone()),
            _ => {
                return Err(ParamError::new(
                    "kind must be one of \"inherits_from\", \"decorates\", \"implements\", \"derives\"",
                ));
            }
        };
        // Relation edges are resolved|ambiguous only (ADR-028/ADR-051): the
        // inferred tier is accepted for parameter parity but adds nothing and
        // must never trigger the `ensure_inferred_*` LLM dispatch.
        let confidence = optional_confidence(arguments)?;
        let limit = optional_usize(arguments, "limit")?
            .unwrap_or(50)
            .clamp(1, 100);
        let offset = parse_cursor_offset(arguments)?;
        let entity_id = match self.resolve_to_locator(&requested_id).await {
            Ok(Some(id)) => id,
            Ok(None) => return Ok(entity_not_found_envelope(&requested_id)),
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };
        let result = self
            .readers
            .with_reader(move |conn| {
                let Some(entity) = entity_by_id(conn, &entity_id)? else {
                    return Ok(entity_not_found_envelope(&entity_id));
                };
                // Refuse to fan out structure around a briefing-blocked entity
                // (same posture as neighborhood, ADR-034).
                if let Some(reason) = crate::briefing_block_reason(&entity) {
                    return Ok(crate::blocked_entity_refusal(&reason));
                }
                // For "both", concatenate the In then Out passes, each edge
                // tagged with the direction that produced it (the neighbor side
                // and the per-relation `direction` field follow that tag). A
                // single-direction request runs exactly one pass, unchanged.
                let mut edges: Vec<(ReferenceDirection, _)> = Vec::new();
                for &edge_dir in direction.passes() {
                    edges.extend(
                        relation_edges_for_entity(conn, &entity.id, edge_dir, kind.as_deref())?
                            .into_iter()
                            .filter(|edge| edge.confidence <= confidence)
                            .map(|edge| (edge_dir, edge)),
                    );
                }
                let total = edges.len();
                let has_more = offset.saturating_add(limit) < total;
                let next_cursor = has_more.then(|| (offset + limit).to_string());
                let mut owner_meta: HashMap<String, crate::OwnerMeta> = HashMap::new();
                let mut file_content: HashMap<String, Option<Vec<u8>>> = HashMap::new();
                // Memoized candidate existence: candidate ids are raw
                // qualname-encoding locators. Under A3 (clarion-719e7320f5) a
                // briefing-blocked candidate keeps its navigable identity, so it
                // may pass — but a candidate that resolves to NO entity row (a
                // phantom alternative) must never be disclosed as a real
                // alternative. So only `NotFound` is filtered; a lookup error
                // still fails closed.
                let mut candidate_visible: HashMap<String, bool> = HashMap::new();
                let mut relations = Vec::new();
                for (edge_dir, edge) in edges.into_iter().skip(offset).take(limit) {
                    let neighbor_id = match edge_dir {
                        ReferenceDirection::In => &edge.from_id,
                        ReferenceDirection::Out => &edge.to_id,
                    };
                    let Some(neighbor) = entity_by_id(conn, neighbor_id)? else {
                        continue;
                    };
                    // A blocked NEIGHBOR is stubbed by entity_json; the anchor
                    // evidence (file, line text — which contains the blocked
                    // declaration) must be withheld with it.
                    let neighbor_blocked = crate::briefing_block_reason(&neighbor).is_some();
                    let (file, anchor, byte_start, byte_end) = if neighbor_blocked {
                        (
                            Value::Null,
                            crate::AnchorLine::redacted("briefing_blocked", true, Value::Null),
                            Value::Null,
                            Value::Null,
                        )
                    } else {
                        // The anchor owner is the edge's own source-file row.
                        let missing_owner = crate::OwnerMeta::missing();
                        let owner = match edge.source_file_id.as_deref() {
                            Some(file_id) => crate::resolve_owner(conn, &mut owner_meta, file_id)?,
                            None => &missing_owner,
                        };
                        let anchor =
                            crate::anchor_line(&mut file_content, owner, edge.source_byte_start);
                        let file = if anchor.briefing_blocked {
                            Value::Null
                        } else {
                            json!(owner.path.as_deref())
                        };
                        (
                            file,
                            anchor,
                            json!(edge.source_byte_start),
                            json!(edge.source_byte_end),
                        )
                    };
                    let candidates: Vec<&String> = edge
                        .candidates
                        .iter()
                        .filter(|cid| {
                            *candidate_visible.entry((*cid).clone()).or_insert_with(|| {
                                matches!(
                                    entity_visibility(conn, cid),
                                    Ok(EntityVisibility::Visible | EntityVisibility::Blocked(_))
                                )
                            })
                        })
                        .collect();
                    relations.push(json!({
                        "kind": edge.kind,
                        "direction": match edge_dir {
                            ReferenceDirection::In => "in",
                            ReferenceDirection::Out => "out",
                        },
                        "entity": entity_json(conn, &neighbor),
                        "edge_confidence": edge.confidence.as_str(),
                        "candidates": candidates,
                        "file": file,
                        "line": anchor.line,
                        "column": anchor.column,
                        "line_text": anchor.line_text,
                        "source_status": anchor.source_status,
                        "briefing_blocked": anchor.briefing_blocked,
                        "drift": anchor.drift,
                        "byte_start": byte_start,
                        "byte_end": byte_end,
                    }));
                }
                Ok(success_envelope(json!({
                    "entity": entity_json(conn, &entity),
                    "direction": direction.as_str(),
                    "filters": {
                        "kind": kind,
                        "confidence": confidence.as_str(),
                    },
                    "relations": relations,
                    "next_cursor": next_cursor,
                    "truncated": has_more,
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
                // ADD an existence gate (clarion-d76e7f7267): `build_call_sites`
                // takes a raw locator with no gate, so a pasted SEI would silently
                // return EMPTY instead of EntityNotFound. Resolve the id-or-SEI to
                // its locator first, then thread the canonical id into the builder.
                let Some(entity) = resolve_entity_ref(conn, &entity_id)? else {
                    return Ok(None);
                };
                build_call_sites(conn, &entity.id, role, kind, confidence, path)
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
