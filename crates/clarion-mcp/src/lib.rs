//! MCP protocol surface for Clarion.

pub mod config;
pub mod filigree;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use clarion_core::{
    EdgeConfidence, INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput,
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID, LeafSummaryPromptInput, LlmProvider, LlmPurpose, LlmRequest,
    build_inferred_calls_prompt, build_leaf_summary_prompt,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::{Mutex as AsyncMutex, broadcast, mpsc, oneshot};

use clarion_core::plugin::{ContentLengthCeiling, Frame, TransportError};
use clarion_storage::{
    CallEdgeMatch, EntityRow, InferredCallEdgeRecord, InferredEdgeCacheEntry, InferredEdgeCacheKey,
    InferredEdgeWriteStats, ReaderPool, StorageError, SummaryCacheEntry, SummaryCacheKey,
    UnresolvedCallSiteRow, WriterCmd, call_edges_from, call_edges_targeting,
    candidate_entities_for_unresolved_sites, child_entity_ids, contained_entity_ids,
    entity_at_line, entity_by_id, find_entities, inferred_edge_cache_key_id,
    inferred_edge_cache_lookup, normalize_source_path, summary_cache_lookup,
    unresolved_call_sites_for_caller, unresolved_callers_for_target,
};

use crate::config::LlmConfig;
use crate::filigree::{EntityAssociation, EntityAssociationsResponse, FiligreeLookup};

/// MCP protocol revision supported by the B.6 stdio server.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const EMPTY_GUIDANCE_FINGERPRINT: &str = "guidance-empty";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[must_use]
pub fn list_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "entity_at",
            description: "Return the innermost Clarion entity whose source range contains a file and line. Paths are normalized relative to the project root. Returns no match rather than guessing when ranges are absent.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file": {"type": "string"},
                    "line": {"type": "integer", "minimum": 1}
                },
                "required": ["file", "line"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "find_entity",
            description: "Search Clarion entities by id, name, short name, and summary text stored on entity rows. Results are paginated and ranked by FTS match where possible. This does not traverse the graph and does not search on-demand summary_cache entries.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "minLength": 1},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                    "cursor": {"type": ["string", "null"]}
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "callers_of",
            description: "Return entities that call the given entity. Default confidence is resolved, so ambiguous static candidates and LLM-inferred edges are excluded unless explicitly requested. Ambiguous edges expand all candidates; inferred edges may trigger bounded LLM dispatch.",
            input_schema: id_confidence_schema(),
        },
        ToolDefinition {
            name: "execution_paths_from",
            description: "Return bounded calls-only execution paths starting at an entity. Default confidence is resolved. max_depth defaults to 3 and traversal also stops at the server edge cap; responses say when they are truncated.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "max_depth": {"type": "integer", "minimum": 1, "maximum": 8},
                    "confidence": confidence_schema()
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "summary",
            description: "Return an on-demand cached summary for one entity. In v0.1 this is leaf scope only: module summaries describe the module docstring and top-level members, not an aggregation of contained function/class summaries.",
            input_schema: id_schema(),
        },
        ToolDefinition {
            name: "issues_for",
            description: "Return Filigree issues attached to this Clarion entity, optionally including issues attached to contained entities. Filigree is an enrichment source; if unavailable, the tool returns an unavailable envelope instead of failing Clarion.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "include_contained": {"type": "boolean"}
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "neighborhood",
            description: "Return the one-hop Clarion neighborhood around an entity: callers, callees, container, contained entities, and references. Default confidence is resolved; ambiguous and inferred calls are opt-in. References are not execution flow.",
            input_schema: id_confidence_schema(),
        },
    ]
}

fn confidence_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["resolved", "ambiguous", "inferred"],
        "default": "resolved"
    })
}

fn id_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "minLength": 1}
        },
        "required": ["id"],
        "additionalProperties": false
    })
}

fn id_confidence_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "minLength": 1},
            "confidence": confidence_schema()
        },
        "required": ["id"],
        "additionalProperties": false
    })
}

#[must_use]
pub fn handle_json_rpc(request: &Value) -> Value {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return error_response(&id, -32600, "invalid request");
    };

    match method {
        "initialize" => result_response(&id, &initialize_result()),
        "tools/list" => result_response(&id, &json!({"tools": list_tools()})),
        "tools/call" => handle_tool_call(&id, request.get("params")),
        _ => error_response(&id, -32601, "method not found"),
    }
}

pub struct ServerState {
    project_root: PathBuf,
    readers: ReaderPool,
    execution_edge_cap: usize,
    summary_llm: Option<SummaryLlmState>,
    clock: Arc<dyn Fn() -> String + Send + Sync>,
    budget: Arc<Mutex<BudgetLedger>>,
    inferred_inflight:
        Arc<AsyncMutex<HashMap<InferredEdgeCacheKey, broadcast::Sender<InferredDispatchOutcome>>>>,
    filigree_client: Option<Arc<dyn FiligreeLookup>>,
}

impl ServerState {
    #[must_use]
    pub fn new(project_root: PathBuf, readers: ReaderPool) -> Self {
        Self {
            project_root,
            readers,
            execution_edge_cap: 500,
            summary_llm: None,
            clock: Arc::new(default_now_string),
            budget: Arc::new(Mutex::new(BudgetLedger::default())),
            inferred_inflight: Arc::new(AsyncMutex::new(HashMap::new())),
            filigree_client: None,
        }
    }

    #[must_use]
    pub fn with_edge_cap(mut self, execution_edge_cap: usize) -> Self {
        self.execution_edge_cap = execution_edge_cap;
        self
    }

    #[must_use]
    pub fn with_summary_llm(
        mut self,
        writer: mpsc::Sender<WriterCmd>,
        config: LlmConfig,
        provider: Arc<dyn LlmProvider>,
    ) -> Self {
        self.summary_llm = Some(SummaryLlmState {
            writer,
            config,
            provider,
        });
        self
    }

    #[must_use]
    pub fn with_clock(mut self, clock: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    #[must_use]
    pub fn with_filigree_client(mut self, client: Arc<dyn FiligreeLookup>) -> Self {
        self.filigree_client = Some(client);
        self
    }

    pub async fn handle_json_rpc(&self, request: &Value) -> Value {
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            return error_response(&id, -32600, "invalid request");
        };

        match method {
            "initialize" => result_response(&id, &initialize_result()),
            "tools/list" => result_response(&id, &json!({"tools": list_tools()})),
            "tools/call" => self.handle_tool_call(&id, request.get("params")).await,
            _ => error_response(&id, -32601, "method not found"),
        }
    }

    async fn handle_tool_call(&self, id: &Value, params: Option<&Value>) -> Value {
        let Some(params) = params.and_then(Value::as_object) else {
            return error_response(id, -32602, "invalid tools/call params");
        };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error_response(id, -32602, "invalid tools/call params: missing name");
        };
        if !list_tools().iter().any(|tool| tool.name == name) {
            return error_response(id, -32601, &format!("unknown tool: {name}"));
        }
        let arguments = params.get("arguments").unwrap_or(&Value::Null);
        let Some(arguments) = arguments.as_object() else {
            return error_response(
                id,
                -32602,
                "invalid tools/call params: arguments must be object",
            );
        };

        let envelope = match name {
            "entity_at" => match self.tool_entity_at(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "find_entity" => match self.tool_find_entity(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "callers_of" => match self.tool_callers_of(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "execution_paths_from" => match self.tool_execution_paths_from(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "neighborhood" => match self.tool_neighborhood(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "summary" => match self.tool_summary(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "issues_for" => match self.tool_issues_for(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            _ => unreachable!("known tools checked above"),
        };

        tool_json_rpc_response(id, &envelope)
    }

    async fn tool_entity_at(
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
                return Ok(tool_error_envelope("invalid-path", &err.to_string(), false));
            }
        };
        let result = self
            .readers
            .with_reader(move |conn| {
                let entity = entity_at_line(conn, &normalized, line)?;
                Ok(json!({"entity": entity.as_ref().map(entity_json)}))
            })
            .await;
        Ok(envelope_from_storage_result(result))
    }

    async fn tool_find_entity(
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
        let result = self
            .readers
            .with_reader(move |conn| {
                let mut rows = find_entities(conn, &pattern, limit.saturating_add(1), offset)?;
                let has_more = rows.len() > limit;
                rows.truncate(limit);
                let next_cursor = if has_more {
                    Some((offset + limit).to_string())
                } else {
                    None
                };
                Ok(json!({
                    "entities": rows.iter().map(entity_json).collect::<Vec<_>>(),
                    "next_cursor": next_cursor
                }))
            })
            .await;
        Ok(envelope_from_storage_result(result))
    }

    async fn tool_callers_of(
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
                        "entity-not-found",
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                }
                let callers = call_edges_targeting(conn, &entity_id, confidence)?
                    .into_iter()
                    .filter_map(|edge| caller_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                Ok(success_envelope_with_stats(
                    json!({"callers": callers}),
                    stats_delta,
                ))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    async fn tool_execution_paths_from(
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
        let result = self
            .readers
            .with_reader(move |conn| {
                if entity_by_id(conn, &entity_id)?.is_none() {
                    return Ok(tool_error_envelope(
                        "entity-not-found",
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                }
                let mut traversal = PathTraversal::new(edge_cap);
                let mut path = vec![entity_id.clone()];
                traversal.walk(conn, &entity_id, &mut path, max_depth, confidence)?;
                let paths = traversal
                    .paths
                    .iter()
                    .map(|path| path_json(conn, path))
                    .collect::<Result<Vec<_>, StorageError>>()?;
                Ok(success_envelope_with_truncation(
                    json!({
                        "paths": paths,
                        "edge_count_visited": traversal.edge_count_visited
                    }),
                    traversal.truncated.then_some("edge-cap"),
                ))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    async fn inferred_execution_paths(&self, entity_id: String, max_depth: usize) -> Value {
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
                    "entity-not-found",
                    &format!("entity {entity_id} was not found"),
                    false,
                );
            }
            Err(err) => return tool_error_envelope("storage-error", &err.to_string(), true),
        }

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
                Err(err) => return tool_error_envelope("storage-error", &err.to_string(), true),
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

        let resolved_paths = self
            .readers
            .with_reader(move |conn| {
                paths
                    .iter()
                    .map(|path| path_json(conn, path))
                    .collect::<Result<Vec<_>, StorageError>>()
            })
            .await;
        match resolved_paths {
            Ok(paths) => success_envelope_with_truncation_and_stats(
                json!({
                    "paths": paths,
                    "edge_count_visited": edge_count_visited
                }),
                truncated.then_some("edge-cap"),
                stats.to_json(),
            ),
            Err(err) => tool_error_envelope("storage-error", &err.to_string(), true),
        }
    }

    async fn tool_neighborhood(
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
                        "entity-not-found",
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
                    .map(entity_json);
                let contained_entities = child_entity_ids(conn, &entity_id)?
                    .iter()
                    .filter_map(|child_id| entity_by_id(conn, child_id).transpose())
                    .map(|row| row.map(|entity| entity_json(&entity)))
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let references_in = reference_neighbors(conn, &entity_id, ReferenceDirection::In)?;
                let references_out =
                    reference_neighbors(conn, &entity_id, ReferenceDirection::Out)?;
                Ok(success_envelope(json!({
                    "entity": entity_json(&entity),
                    "callers": inbound_callers,
                    "callees": outbound_calls,
                    "container": container_entity,
                    "contained": contained_entities,
                    "references_in": references_in,
                    "references_out": references_out
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    async fn tool_issues_for(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let include_contained = optional_bool(arguments, "include_contained")?.unwrap_or(true);
        let Some(client) = self.filigree_client.clone() else {
            return Ok(issues_unavailable(
                "filigree-disabled",
                "Filigree integration is disabled",
            ));
        };
        let read = match self
            .read_issues_for_entities(entity_id, include_contained)
            .await
        {
            Ok(Some(read)) => read,
            Ok(None) => {
                return Ok(issues_unavailable(
                    "entity-not-found",
                    "Clarion entity was not found",
                ));
            }
            Err(err) => return Ok(tool_error_envelope("storage-error", &err.to_string(), true)),
        };
        let mut accumulator = IssuesForAccumulator::new(&read.entities);
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
                    return Ok(issues_unavailable("filigree-unreachable", &err.to_string()));
                }
                Err(err) => {
                    return Ok(issues_unavailable(
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
        Ok(accumulator.into_envelope(read.entity_cap_truncated, requests_total))
    }

    async fn read_issues_for_entities(
        &self,
        entity_id: String,
        include_contained: bool,
    ) -> Result<Option<IssuesForRead>, StorageError> {
        self.readers
            .with_reader(move |conn| {
                let Some(root) = entity_by_id(conn, &entity_id)? else {
                    return Ok(None);
                };
                let mut ids = vec![root.id.clone()];
                let mut entity_cap_truncated = false;
                if include_contained {
                    let contained = contained_entity_ids(conn, &entity_id, 1_000)?;
                    entity_cap_truncated = contained.truncated;
                    ids.extend(contained.entity_ids);
                }
                let mut entities = Vec::with_capacity(ids.len());
                for id in ids {
                    if let Some(entity) = entity_by_id(conn, &id)? {
                        entities.push(entity);
                    }
                }
                Ok(Some(IssuesForRead {
                    entities,
                    entity_cap_truncated,
                }))
            })
            .await
    }

    async fn tool_summary(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let now = (self.clock)();
        let read = match self
            .read_summary_inputs(entity_id, self.summary_model_id())
            .await
        {
            Ok(read) => read,
            Err(err) => return Ok(tool_error_envelope("storage-error", &err.to_string(), true)),
        };

        let SummaryRead::Ready(ready) = read else {
            return Ok(summary_read_error(read));
        };

        if let Some(envelope) = self.cached_summary_envelope(&ready, &now).await {
            return Ok(envelope);
        }

        if self.summary_budget_blocked() {
            return Ok(cost_ceiling_envelope(
                "LLM session cost ceiling has been reached",
            ));
        }

        let Some(summary_llm) = &self.summary_llm else {
            return Ok(tool_error_envelope(
                "llm-disabled",
                "LLM summaries are disabled and no fresh cache row is available",
                false,
            ));
        };
        if !summary_llm.config.enabled {
            return Ok(tool_error_envelope(
                "llm-disabled",
                "LLM summaries are disabled and no fresh cache row is available",
                false,
            ));
        }

        Ok(self.refresh_summary(*ready, summary_llm, now).await)
    }

    async fn ensure_inferred_for_target(
        &self,
        target_id: &str,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let target_id = target_id.to_owned();
        let caller_ids = self
            .readers
            .with_reader(move |conn| {
                let Some(target) = entity_by_id(conn, &target_id)? else {
                    return Ok(Vec::new());
                };
                let sites = unresolved_callers_for_target(conn, &target, 50)?;
                let mut seen = std::collections::BTreeSet::new();
                Ok(sites
                    .into_iter()
                    .filter_map(|site| {
                        if seen.insert(site.caller_entity_id.clone()) {
                            Some(site.caller_entity_id)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>())
            })
            .await
            .map_err(|err| InferredDispatchFailure::from_storage(&err))?;

        let mut stats = InferredDispatchStats {
            candidate_callers_considered: u64::try_from(caller_ids.len()).unwrap_or(u64::MAX),
            ..InferredDispatchStats::default()
        };
        for caller_id in caller_ids {
            stats.merge(&self.ensure_inferred_for_caller(&caller_id).await?);
        }
        Ok(stats)
    }

    async fn ensure_inferred_for_caller(
        &self,
        caller_id: &str,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let model_id = self.inferred_edges_model_id();
        let Some(read) = self
            .read_inferred_inputs(caller_id.to_owned(), model_id)
            .await?
        else {
            return Ok(InferredDispatchStats::default());
        };

        if let Some(cached) = read.cached.clone() {
            return self.materialize_cached_inferred(read, cached).await;
        }

        if self.summary_budget_blocked() {
            return Err(InferredDispatchFailure::new(
                "cost-ceiling-exceeded",
                "LLM session cost ceiling has been reached",
                false,
            ));
        }
        let Some(llm) = self.inference_llm_snapshot() else {
            return Err(InferredDispatchFailure::new(
                "llm-disabled",
                "LLM inferred-edge dispatch is disabled and no cache row is available",
                false,
            ));
        };
        if !llm.config.enabled {
            return Err(InferredDispatchFailure::new(
                "llm-disabled",
                "LLM inferred-edge dispatch is disabled and no cache row is available",
                false,
            ));
        }

        self.coalesced_inferred_dispatch(read.key.clone(), read, llm)
            .await
    }

    async fn read_inferred_inputs(
        &self,
        caller_id: String,
        model_id: String,
    ) -> Result<Option<InferredRead>, InferredDispatchFailure> {
        self.readers
            .with_reader(move |conn| {
                let Some(caller) = entity_by_id(conn, &caller_id)? else {
                    return Ok(None);
                };
                let Some(content_hash) = caller.content_hash.clone() else {
                    return Ok(None);
                };
                let sites = unresolved_call_sites_for_caller(conn, &caller_id, 100)?;
                if sites.is_empty() {
                    return Ok(None);
                }
                let candidates = candidate_entities_for_unresolved_sites(conn, &sites, 100)?;
                let key = InferredEdgeCacheKey {
                    caller_entity_id: caller.id.clone(),
                    caller_content_hash: content_hash,
                    model_id,
                    prompt_version: INFERRED_CALLS_PROMPT_VERSION.to_owned(),
                };
                let cached = inferred_edge_cache_lookup(conn, &key)?;
                Ok(Some(InferredRead {
                    caller,
                    sites,
                    candidates,
                    key,
                    cached,
                }))
            })
            .await
            .map_err(|err| InferredDispatchFailure::from_storage(&err))
    }

    async fn materialize_cached_inferred(
        &self,
        read: InferredRead,
        mut cached: InferredEdgeCacheEntry,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let Some(llm) = self.inference_llm_snapshot() else {
            return Err(InferredDispatchFailure::new(
                "llm-disabled",
                "LLM inferred-edge dispatch is disabled and no writer is available",
                false,
            ));
        };
        let now = (self.clock)();
        cached.last_accessed_at = now;
        let edges = inferred_records_from_result(
            &read,
            &cached.result_json,
            self.max_inferred_edges_per_caller(),
        )?;
        let write = self
            .send_writer(&llm.writer, |ack| WriterCmd::InsertInferredEdges {
                cache_entry: Box::new(cached),
                edges,
                ack,
            })
            .await
            .map_err(|err| InferredDispatchFailure::from_storage(&err))?;
        Ok(InferredDispatchStats::cache_hit(write))
    }

    async fn coalesced_inferred_dispatch(
        &self,
        key: InferredEdgeCacheKey,
        read: InferredRead,
        llm: InferenceLlmState,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let maybe_rx = {
            let mut in_flight = self.inferred_inflight.lock().await;
            if let Some(sender) = in_flight.get(&key) {
                Some(sender.subscribe())
            } else {
                let (sender, _) = broadcast::channel(8);
                in_flight.insert(key.clone(), sender);
                None
            }
        };

        if let Some(mut rx) = maybe_rx {
            return match tokio::time::timeout(std::time::Duration::from_secs(60), rx.recv()).await {
                Ok(Ok(outcome)) => {
                    let mut stats = outcome.into_result()?;
                    stats.coalesced_waits_total += 1;
                    Ok(stats)
                }
                Ok(Err(_)) => Err(InferredDispatchFailure::new(
                    "inferred-dispatch-cancelled",
                    "inferred dispatch owner ended before broadcasting a result",
                    true,
                )),
                Err(_) => Err(InferredDispatchFailure::new(
                    "inferred-dispatch-timeout",
                    "timed out waiting for in-flight inferred dispatch",
                    true,
                )),
            };
        }

        let outcome =
            InferredDispatchOutcome::from_result(self.perform_inferred_dispatch(read, &llm).await);
        if let Some(sender) = self.inferred_inflight.lock().await.remove(&key) {
            let _ = sender.send(outcome.clone());
        }
        outcome.into_result()
    }

    async fn perform_inferred_dispatch(
        &self,
        read: InferredRead,
        llm: &InferenceLlmState,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let prompt = build_inferred_calls_prompt(&InferredCallsPromptInput {
            caller_entity_id: read.caller.id.clone(),
            caller_source_excerpt: source_excerpt(&read.caller),
            unresolved_call_sites_json: unresolved_sites_json(&read.sites),
            candidate_entities_json: entities_json(&read.candidates),
        });
        let request = LlmRequest {
            purpose: LlmPurpose::InferredEdges,
            model_id: read.key.model_id.clone(),
            prompt_id: prompt.id.to_owned(),
            prompt: prompt.body,
            max_output_tokens: 512,
        };
        let Some(reservation) = self.reserve_budget(
            llm.provider.estimate_cost_usd(&request),
            llm.config.session_cost_ceiling_usd,
        ) else {
            return Err(InferredDispatchFailure::new(
                "cost-ceiling-exceeded",
                "LLM session cost ceiling has been reached",
                false,
            ));
        };
        let response = llm.provider.invoke(request).map_err(|err| {
            InferredDispatchFailure::new("llm-provider-error", &err.to_string(), true)
        })?;
        if !reservation.commit(response.cost_usd, llm.config.session_cost_ceiling_usd) {
            return Err(InferredDispatchFailure::new(
                "cost-ceiling-exceeded",
                "LLM session cost ceiling has been reached",
                false,
            ));
        }
        let edges = inferred_records_from_result(
            &read,
            &response.output_json,
            self.max_inferred_edges_per_caller(),
        )?;
        let now = (self.clock)();
        let entry = InferredEdgeCacheEntry {
            key: read.key,
            result_json: response.output_json,
            cost_usd: response.cost_usd,
            token_count: i64::from(response.input_tokens) + i64::from(response.output_tokens),
            created_at: now.clone(),
            last_accessed_at: now,
        };
        let write = self
            .send_writer(&llm.writer, |ack| WriterCmd::InsertInferredEdges {
                cache_entry: Box::new(entry.clone()),
                edges,
                ack,
            })
            .await
            .map_err(|err| InferredDispatchFailure::from_storage(&err))?;
        Ok(InferredDispatchStats::cache_miss(
            write,
            entry.cost_usd,
            entry.token_count,
        ))
    }

    async fn read_summary_inputs(
        &self,
        entity_id: String,
        summary_model_id: String,
    ) -> Result<SummaryRead, StorageError> {
        self.readers
            .with_reader(move |conn| {
                let Some(entity) = entity_by_id(conn, &entity_id)? else {
                    return Ok(SummaryRead::EntityNotFound(entity_id));
                };
                let Some(content_hash) = entity.content_hash.clone() else {
                    return Ok(SummaryRead::MissingContentHash(entity.id));
                };
                let key = SummaryCacheKey {
                    entity_id: entity.id.clone(),
                    content_hash,
                    prompt_template_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                    model_tier: summary_model_id,
                    guidance_fingerprint: EMPTY_GUIDANCE_FINGERPRINT.to_owned(),
                };
                let cached = summary_cache_lookup(conn, &key)?;
                let caller_count = i64::try_from(
                    call_edges_targeting(conn, &entity.id, EdgeConfidence::Ambiguous)?.len(),
                )
                .unwrap_or(i64::MAX);
                let fan_out = i64::try_from(
                    call_edges_from(conn, &entity.id, EdgeConfidence::Ambiguous)?.len(),
                )
                .unwrap_or(i64::MAX);
                Ok(SummaryRead::Ready(Box::new(SummaryReady {
                    entity,
                    key,
                    cached,
                    caller_count,
                    fan_out,
                })))
            })
            .await
    }

    async fn cached_summary_envelope(&self, ready: &SummaryReady, now: &str) -> Option<Value> {
        let cached = ready.cached.as_ref()?;
        if summary_cache_expired(&cached.created_at, now, self.summary_cache_max_age_days()) {
            return None;
        }
        if let Some(summary_llm) = &self.summary_llm
            && let Err(err) = self
                .send_writer(&summary_llm.writer, |ack| WriterCmd::TouchSummaryCache {
                    key: ready.key.clone(),
                    last_accessed_at: now.to_owned(),
                    ack,
                })
                .await
        {
            return Some(tool_error_envelope("storage-error", &err.to_string(), true));
        }
        Some(summary_success_envelope(
            &ready.entity,
            cached,
            true,
            stale_semantic(cached, ready.caller_count, ready.fan_out),
            json!({"summary_cache_hits_total": 1}),
        ))
    }

    async fn refresh_summary(
        &self,
        ready: SummaryReady,
        summary_llm: &SummaryLlmState,
        now: String,
    ) -> Value {
        let model_id = self.summary_model_id();
        let prompt = build_leaf_summary_prompt(&LeafSummaryPromptInput {
            entity_id: ready.entity.id.clone(),
            kind: ready.entity.kind.clone(),
            name: ready.entity.name.clone(),
            source_excerpt: source_excerpt(&ready.entity),
        });
        let request = LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: model_id.clone(),
            prompt_id: prompt.id.to_owned(),
            prompt: prompt.body,
            max_output_tokens: 512,
        };
        let Some(reservation) = self.reserve_budget(
            summary_llm.provider.estimate_cost_usd(&request),
            summary_llm.config.session_cost_ceiling_usd,
        ) else {
            return cost_ceiling_envelope("LLM session cost ceiling has been reached");
        };
        let response = match summary_llm.provider.invoke(request) {
            Ok(response) => response,
            Err(err) => {
                return tool_error_envelope("llm-provider-error", &err.to_string(), true);
            }
        };

        if !reservation.commit(
            response.cost_usd,
            summary_llm.config.session_cost_ceiling_usd,
        ) {
            return cost_ceiling_envelope("LLM session cost ceiling has been reached");
        }

        if serde_json::from_str::<Value>(&response.output_json).is_err() {
            return tool_error_envelope(
                "llm-invalid-json",
                "summary provider returned non-JSON output",
                true,
            );
        }

        let entry = SummaryCacheEntry {
            key: ready.key,
            summary_json: response.output_json,
            cost_usd: response.cost_usd,
            tokens_input: i64::from(response.input_tokens),
            tokens_output: i64::from(response.output_tokens),
            caller_count: ready.caller_count,
            fan_out: ready.fan_out,
            stale_semantic: false,
            created_at: now.clone(),
            last_accessed_at: now,
        };
        if let Err(err) = self
            .send_writer(&summary_llm.writer, |ack| WriterCmd::UpsertSummaryCache {
                entry: Box::new(entry.clone()),
                ack,
            })
            .await
        {
            return tool_error_envelope("storage-error", &err.to_string(), true);
        }

        summary_success_envelope(
            &ready.entity,
            &entry,
            false,
            false,
            json!({
                "summary_cache_misses_total": 1,
                "summary_llm_cost_usd": entry.cost_usd,
                "summary_tokens_input": entry.tokens_input,
                "summary_tokens_output": entry.tokens_output
            }),
        )
    }

    async fn send_writer<T>(
        &self,
        writer: &mpsc::Sender<WriterCmd>,
        build: impl FnOnce(oneshot::Sender<Result<T, StorageError>>) -> WriterCmd,
    ) -> Result<T, StorageError>
    where
        T: Send + 'static,
    {
        let (ack_tx, ack_rx) = oneshot::channel();
        writer
            .send(build(ack_tx))
            .await
            .map_err(|_| StorageError::WriterGone)?;
        ack_rx.await.map_err(|_| StorageError::WriterNoResponse)?
    }

    fn summary_budget_blocked(&self) -> bool {
        self.budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .blocked
    }

    fn reserve_budget(&self, estimate_usd: f64, ceiling_usd: f64) -> Option<BudgetReservation> {
        let estimate_usd = estimate_usd.max(0.0);
        let mut budget = self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if budget.blocked || budget.spent_usd + budget.reserved_usd + estimate_usd > ceiling_usd {
            budget.blocked = true;
            return None;
        }
        budget.reserved_usd += estimate_usd;
        Some(BudgetReservation {
            budget: Arc::clone(&self.budget),
            amount_usd: estimate_usd,
            active: true,
        })
    }

    fn inference_llm_snapshot(&self) -> Option<InferenceLlmState> {
        self.summary_llm.as_ref().map(|llm| InferenceLlmState {
            writer: llm.writer.clone(),
            config: llm.config.clone(),
            provider: Arc::clone(&llm.provider),
        })
    }

    fn summary_cache_max_age_days(&self) -> u32 {
        self.summary_llm
            .as_ref()
            .map_or(180, |summary| summary.config.cache_max_age_days)
    }

    fn summary_model_id(&self) -> String {
        self.summary_llm.as_ref().map_or_else(
            || "claude-haiku-4-5".to_owned(),
            |summary| {
                summary
                    .provider
                    .tier_to_model("summary")
                    .unwrap_or(&summary.config.summary_model_id)
                    .to_owned()
            },
        )
    }

    fn inferred_edges_model_id(&self) -> String {
        self.summary_llm.as_ref().map_or_else(
            || "claude-haiku-4-5".to_owned(),
            |summary| {
                summary
                    .provider
                    .tier_to_model("inferred_edges")
                    .unwrap_or(&summary.config.inferred_edges_model_id)
                    .to_owned()
            },
        )
    }

    fn max_inferred_edges_per_caller(&self) -> usize {
        self.summary_llm.as_ref().map_or(8, |summary| {
            usize::try_from(summary.config.max_inferred_edges_per_caller).unwrap_or(8)
        })
    }
}

struct SummaryLlmState {
    writer: mpsc::Sender<WriterCmd>,
    config: LlmConfig,
    provider: Arc<dyn LlmProvider>,
}

#[derive(Default)]
struct BudgetLedger {
    spent_usd: f64,
    reserved_usd: f64,
    blocked: bool,
}

struct BudgetReservation {
    budget: Arc<Mutex<BudgetLedger>>,
    amount_usd: f64,
    active: bool,
}

impl BudgetReservation {
    fn commit(mut self, actual_cost_usd: f64, ceiling_usd: f64) -> bool {
        let actual_cost_usd = actual_cost_usd.max(0.0);
        let mut budget = self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.active {
            budget.reserved_usd = (budget.reserved_usd - self.amount_usd).max(0.0);
            self.active = false;
        }
        if budget.blocked || budget.spent_usd + actual_cost_usd > ceiling_usd {
            budget.blocked = true;
            return false;
        }
        budget.spent_usd += actual_cost_usd;
        true
    }
}

impl Drop for BudgetReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut budget = self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        budget.reserved_usd = (budget.reserved_usd - self.amount_usd).max(0.0);
        self.active = false;
    }
}

enum SummaryRead {
    Ready(Box<SummaryReady>),
    EntityNotFound(String),
    MissingContentHash(String),
}

struct SummaryReady {
    entity: EntityRow,
    key: SummaryCacheKey,
    cached: Option<SummaryCacheEntry>,
    caller_count: i64,
    fan_out: i64,
}

struct IssuesForRead {
    entities: Vec<EntityRow>,
    entity_cap_truncated: bool,
}

struct IssuesForAccumulator {
    entities_by_id: HashMap<String, EntityRow>,
    seen_issue_ids: BTreeSet<String>,
    matched: Vec<Value>,
    drifted: Vec<Value>,
    not_found: Vec<Value>,
    diagnostics: Vec<Value>,
    emitted: usize,
    issue_cap_truncated: bool,
}

impl IssuesForAccumulator {
    fn new(entities: &[EntityRow]) -> Self {
        Self {
            entities_by_id: entities
                .iter()
                .map(|entity| (entity.id.clone(), entity.clone()))
                .collect(),
            seen_issue_ids: BTreeSet::new(),
            matched: Vec::new(),
            drifted: Vec::new(),
            not_found: Vec::new(),
            diagnostics: Vec::new(),
            emitted: 0,
            issue_cap_truncated: false,
        }
    }

    fn add_response(&mut self, response: EntityAssociationsResponse) {
        for association in response.associations {
            if self.emitted >= 100 {
                self.issue_cap_truncated = true;
                break;
            }
            if !self.seen_issue_ids.insert(association.issue_id.clone()) {
                continue;
            }
            self.emitted += 1;
            self.add_association(&association);
        }
    }

    fn add_association(&mut self, association: &EntityAssociation) {
        match self.entities_by_id.get(&association.clarion_entity_id) {
            None => self
                .not_found
                .push(association_json(association, None, None, "not_found")),
            Some(entity) => match entity.content_hash.as_deref() {
                Some(current_hash) if current_hash == association.content_hash_at_attach => {
                    self.matched.push(association_json(
                        association,
                        Some(entity),
                        Some(current_hash),
                        "matched",
                    ));
                }
                Some(current_hash) => {
                    self.drifted.push(association_json(
                        association,
                        Some(entity),
                        Some(current_hash),
                        "drifted",
                    ));
                }
                None => {
                    self.diagnostics.push(json!({
                        "code": "CLA-ENTITY-CONTENT-HASH-MISSING",
                        "entity_id": entity.id
                    }));
                    self.matched
                        .push(association_json(association, Some(entity), None, "unknown"));
                }
            },
        }
    }

    fn into_envelope(self, entity_cap_truncated: bool, requests_total: usize) -> Value {
        let truncation_reason = if self.issue_cap_truncated {
            Some("issue-cap")
        } else {
            entity_cap_truncated.then_some("entity-cap")
        };
        let mut envelope = success_envelope_with_truncation_and_stats(
            json!({
                "available": true,
                "matched": self.matched,
                "drifted": self.drifted,
                "not_found": self.not_found
            }),
            truncation_reason,
            json!({
                "filigree_requests_total": requests_total,
                "filigree_issues_returned_total": self.emitted
            }),
        );
        if let Some(object) = envelope.as_object_mut()
            && !self.diagnostics.is_empty()
        {
            object.insert("diagnostics".to_owned(), Value::Array(self.diagnostics));
        }
        envelope
    }
}

#[derive(Clone)]
struct InferenceLlmState {
    writer: mpsc::Sender<WriterCmd>,
    config: LlmConfig,
    provider: Arc<dyn LlmProvider>,
}

#[derive(Clone)]
struct InferredRead {
    caller: EntityRow,
    sites: Vec<UnresolvedCallSiteRow>,
    candidates: Vec<EntityRow>,
    key: InferredEdgeCacheKey,
    cached: Option<InferredEdgeCacheEntry>,
}

#[derive(Debug, Clone, Default)]
struct InferredDispatchStats {
    cache_hits_total: u64,
    cache_misses_total: u64,
    edges_materialized_total: u64,
    edges_skipped_static_duplicates_total: u64,
    candidate_callers_considered: u64,
    coalesced_waits_total: u64,
    llm_cost_usd: f64,
    tokens_total: i64,
}

impl InferredDispatchStats {
    fn cache_hit(write: InferredEdgeWriteStats) -> Self {
        Self {
            cache_hits_total: 1,
            edges_materialized_total: write.inserted_edges,
            edges_skipped_static_duplicates_total: write.skipped_static_duplicates,
            ..Self::default()
        }
    }

    fn cache_miss(write: InferredEdgeWriteStats, cost_usd: f64, tokens: i64) -> Self {
        Self {
            cache_misses_total: 1,
            edges_materialized_total: write.inserted_edges,
            edges_skipped_static_duplicates_total: write.skipped_static_duplicates,
            llm_cost_usd: cost_usd,
            tokens_total: tokens,
            ..Self::default()
        }
    }

    fn merge(&mut self, other: &Self) {
        self.cache_hits_total += other.cache_hits_total;
        self.cache_misses_total += other.cache_misses_total;
        self.edges_materialized_total += other.edges_materialized_total;
        self.edges_skipped_static_duplicates_total += other.edges_skipped_static_duplicates_total;
        self.candidate_callers_considered += other.candidate_callers_considered;
        self.coalesced_waits_total += other.coalesced_waits_total;
        self.llm_cost_usd += other.llm_cost_usd;
        self.tokens_total += other.tokens_total;
    }

    fn to_json(&self) -> Value {
        json!({
            "inferred_dispatch_cache_hits_total": self.cache_hits_total,
            "inferred_dispatch_misses_total": self.cache_misses_total,
            "inferred_edges_materialized_total": self.edges_materialized_total,
            "inferred_edges_skipped_static_duplicates_total": self.edges_skipped_static_duplicates_total,
            "inferred_candidate_callers_considered": self.candidate_callers_considered,
            "inferred_dispatch_coalesced_total": self.coalesced_waits_total,
            "inferred_llm_cost_usd": self.llm_cost_usd,
            "inferred_tokens_total": self.tokens_total
        })
    }
}

#[derive(Debug, Clone)]
struct InferredDispatchFailure {
    code: &'static str,
    message: String,
    retryable: bool,
}

impl InferredDispatchFailure {
    fn new(code: &'static str, message: &str, retryable: bool) -> Self {
        Self {
            code,
            message: message.to_owned(),
            retryable,
        }
    }

    fn from_storage(err: &StorageError) -> Self {
        Self {
            code: "storage-error",
            message: err.to_string(),
            retryable: true,
        }
    }

    fn to_envelope(&self) -> Value {
        if self.code == "cost-ceiling-exceeded" {
            return cost_ceiling_envelope(&self.message);
        }
        tool_error_envelope(self.code, &self.message, self.retryable)
    }
}

#[derive(Debug, Clone)]
enum InferredDispatchOutcome {
    Ok(InferredDispatchStats),
    Err(InferredDispatchFailure),
}

impl InferredDispatchOutcome {
    fn from_result(result: Result<InferredDispatchStats, InferredDispatchFailure>) -> Self {
        match result {
            Ok(stats) => Self::Ok(stats),
            Err(err) => Self::Err(err),
        }
    }

    fn into_result(self) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        match self {
            Self::Ok(stats) => Ok(stats),
            Self::Err(err) => Err(err),
        }
    }
}

#[derive(Debug, Deserialize)]
struct InferredCallsResponse {
    #[serde(default)]
    edges: Vec<InferredCallsResponseEdge>,
}

#[derive(Debug, Deserialize)]
struct InferredCallsResponseEdge {
    site_key: Option<String>,
    target_id: String,
    confidence: Option<f64>,
    rationale: Option<String>,
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("invalid JSON-RPC frame body: {0}")]
    Json(#[from] serde_json::Error),

    #[error("MCP transport error: {0}")]
    Transport(#[from] TransportError),

    #[error("MCP runtime error: {0}")]
    Runtime(#[from] std::io::Error),
}

pub fn handle_frame(frame: &Frame) -> Result<Frame, McpError> {
    let request = serde_json::from_slice(&frame.body)?;
    let response = handle_json_rpc(&request);
    Ok(Frame {
        body: serde_json::to_vec(&response)?,
    })
}

pub async fn handle_frame_with_state(
    state: &ServerState,
    frame: &Frame,
) -> Result<Frame, McpError> {
    let request = serde_json::from_slice(&frame.body)?;
    let response = state.handle_json_rpc(&request).await;
    Ok(Frame {
        body: serde_json::to_vec(&response)?,
    })
}

pub fn serve_stdio(
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
) -> Result<(), McpError> {
    loop {
        let frame = match clarion_core::plugin::read_frame(reader, ContentLengthCeiling::DEFAULT) {
            Ok(frame) => frame,
            Err(TransportError::Io(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        };
        let response = handle_frame(&frame)?;
        clarion_core::plugin::write_frame(writer, &response)?;
    }
}

pub fn serve_stdio_with_state(
    state: &ServerState,
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
) -> Result<(), McpError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    serve_stdio_with_state_on_runtime(&runtime, state, reader, writer)
}

pub fn serve_stdio_with_state_on_runtime(
    runtime: &tokio::runtime::Runtime,
    state: &ServerState,
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
) -> Result<(), McpError> {
    let _guard = runtime.enter();
    loop {
        let frame = match clarion_core::plugin::read_frame(reader, ContentLengthCeiling::DEFAULT) {
            Ok(frame) => frame,
            Err(TransportError::Io(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        };
        let response = runtime.block_on(handle_frame_with_state(state, &frame))?;
        clarion_core::plugin::write_frame(writer, &response)?;
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": {}
        },
        "serverInfo": {
            "name": "clarion",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn handle_tool_call(id: &Value, params: Option<&Value>) -> Value {
    let Some(params) = params.and_then(Value::as_object) else {
        return error_response(id, -32602, "invalid tools/call params");
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return error_response(id, -32602, "invalid tools/call params: missing name");
    };
    if !list_tools().iter().any(|tool| tool.name == name) {
        return error_response(id, -32601, &format!("unknown tool: {name}"));
    }

    result_response(
        id,
        &json!({
            "content": [
                {
                    "type": "text",
                    "text": serde_json::to_string(&json!({
                        "ok": false,
                        "result": null,
                        "error": {
                            "code": "tool-unimplemented",
                            "message": format!("{name} is not implemented yet"),
                            "retryable": false
                        },
                        "diagnostics": [],
                        "truncated": false,
                        "truncation_reason": null,
                        "stats_delta": {}
                    })).expect("tool error envelope serializes")
                }
            ],
            "isError": true
        }),
    )
}

#[derive(Debug)]
struct ParamError {
    message: String,
}

impl ParamError {
    fn new(message: &str) -> Self {
        Self {
            message: message.to_owned(),
        }
    }

    fn to_json_rpc(&self, id: &Value) -> Value {
        error_response(id, -32602, &self.message)
    }
}

struct PathTraversal {
    edge_cap: usize,
    edge_count_visited: usize,
    truncated: bool,
    paths: Vec<Vec<String>>,
}

impl PathTraversal {
    fn new(edge_cap: usize) -> Self {
        Self {
            edge_cap,
            edge_count_visited: 0,
            truncated: false,
            paths: Vec::new(),
        }
    }

    fn walk(
        &mut self,
        conn: &rusqlite::Connection,
        current_id: &str,
        path: &mut Vec<String>,
        remaining_depth: usize,
        confidence: EdgeConfidence,
    ) -> Result<(), StorageError> {
        if remaining_depth == 0 || self.truncated {
            return Ok(());
        }
        for edge in call_edges_from(conn, current_id, confidence)? {
            self.edge_count_visited += 1;
            if self.edge_count_visited > self.edge_cap {
                self.truncated = true;
                return Ok(());
            }
            if path.iter().any(|seen| seen == &edge.to_id) {
                continue;
            }
            path.push(edge.to_id.clone());
            self.paths.push(path.clone());
            self.walk(conn, &edge.to_id, path, remaining_depth - 1, confidence)?;
            path.pop();
            if self.truncated {
                return Ok(());
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ReferenceDirection {
    In,
    Out,
}

fn required_str<'a>(
    arguments: &'a serde_json::Map<String, Value>,
    field: &str,
) -> std::result::Result<&'a str, ParamError> {
    arguments
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ParamError::new(&format!("{field} must be a non-empty string")))
}

fn required_i64(
    arguments: &serde_json::Map<String, Value>,
    field: &str,
) -> std::result::Result<i64, ParamError> {
    arguments
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| ParamError::new(&format!("{field} must be an integer")))
}

fn optional_usize(
    arguments: &serde_json::Map<String, Value>,
    field: &str,
) -> std::result::Result<Option<usize>, ParamError> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
    let Some(raw) = value.as_u64() else {
        return Err(ParamError::new(&format!(
            "{field} must be a non-negative integer"
        )));
    };
    usize::try_from(raw)
        .map(Some)
        .map_err(|_| ParamError::new(&format!("{field} is too large")))
}

fn optional_bool(
    arguments: &serde_json::Map<String, Value>,
    field: &str,
) -> std::result::Result<Option<bool>, ParamError> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| ParamError::new(&format!("{field} must be a boolean")))
}

fn optional_confidence(
    arguments: &serde_json::Map<String, Value>,
) -> std::result::Result<EdgeConfidence, ParamError> {
    match arguments.get("confidence").and_then(Value::as_str) {
        None | Some("resolved") => Ok(EdgeConfidence::Resolved),
        Some("ambiguous") => Ok(EdgeConfidence::Ambiguous),
        Some("inferred") => Ok(EdgeConfidence::Inferred),
        Some(_) => Err(ParamError::new(
            "confidence must be one of resolved, ambiguous, inferred",
        )),
    }
}

fn envelope_from_storage_result(result: Result<Value, StorageError>) -> Value {
    match result {
        Ok(result) => success_envelope(result),
        Err(err) => tool_error_envelope("storage-error", &err.to_string(), true),
    }
}

fn flatten_storage_envelope_result(result: Result<Value, StorageError>) -> Value {
    match result {
        Ok(envelope) => envelope,
        Err(err) => tool_error_envelope("storage-error", &err.to_string(), true),
    }
}

fn success_envelope(result: Value) -> Value {
    success_envelope_with_truncation(result, None)
}

fn success_envelope_with_truncation(result: Value, truncation_reason: Option<&str>) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert("ok".to_owned(), Value::Bool(true));
    envelope.insert("result".to_owned(), result);
    envelope.insert("error".to_owned(), Value::Null);
    envelope.insert("diagnostics".to_owned(), Value::Array(Vec::new()));
    envelope.insert(
        "truncated".to_owned(),
        Value::Bool(truncation_reason.is_some()),
    );
    envelope.insert(
        "truncation_reason".to_owned(),
        truncation_reason.map_or(Value::Null, |reason| Value::String(reason.to_owned())),
    );
    envelope.insert("stats_delta".to_owned(), json!({}));
    Value::Object(envelope)
}

fn success_envelope_with_truncation_and_stats(
    result: Value,
    truncation_reason: Option<&str>,
    stats_delta: Value,
) -> Value {
    let mut envelope = success_envelope_with_truncation(result, truncation_reason);
    if let Some(object) = envelope.as_object_mut() {
        object.insert("stats_delta".to_owned(), stats_delta);
    }
    envelope
}

fn success_envelope_with_stats(result: Value, stats_delta: Value) -> Value {
    let mut envelope = success_envelope(result);
    if let Some(object) = envelope.as_object_mut() {
        object.insert("stats_delta".to_owned(), stats_delta);
    }
    envelope
}

fn tool_error_envelope(code: &str, message: &str, retryable: bool) -> Value {
    json!({
        "ok": false,
        "result": null,
        "error": {
            "code": code,
            "message": message,
            "retryable": retryable
        },
        "diagnostics": [],
        "truncated": false,
        "truncation_reason": null,
        "stats_delta": {}
    })
}

fn cost_ceiling_envelope(message: &str) -> Value {
    json!({
        "ok": false,
        "result": null,
        "error": {
            "code": "cost-ceiling-exceeded",
            "message": message,
            "retryable": false
        },
        "diagnostics": [
            {
                "code": "CLA-LLM-COST-CEILING-EXCEEDED",
                "message": message
            }
        ],
        "truncated": false,
        "truncation_reason": null,
        "stats_delta": {
            "cost_ceiling_exceeded_total": 1
        }
    })
}

fn issues_unavailable(reason: &str, message: &str) -> Value {
    success_envelope(json!({
        "available": false,
        "reason": reason,
        "message": message,
        "matched": [],
        "drifted": [],
        "not_found": []
    }))
}

fn association_json(
    association: &EntityAssociation,
    entity: Option<&EntityRow>,
    current_content_hash: Option<&str>,
    drift_status: &str,
) -> Value {
    json!({
        "issue_id": association.issue_id,
        "entity_id": association.clarion_entity_id,
        "entity": entity.map(entity_json),
        "content_hash_at_attach": association.content_hash_at_attach,
        "current_content_hash": current_content_hash,
        "attached_at": association.attached_at,
        "attached_by": association.attached_by,
        "drift_status": drift_status
    })
}

fn summary_read_error(read: SummaryRead) -> Value {
    match read {
        SummaryRead::EntityNotFound(id) => tool_error_envelope(
            "entity-not-found",
            &format!("entity {id} was not found"),
            false,
        ),
        SummaryRead::MissingContentHash(id) => tool_error_envelope(
            "content-hash-missing",
            &format!("entity {id} has no content hash for summary cache keying"),
            false,
        ),
        SummaryRead::Ready(_) => unreachable!("ready summary read is not an error"),
    }
}

fn summary_success_envelope(
    entity: &EntityRow,
    entry: &SummaryCacheEntry,
    cache_hit: bool,
    stale_semantic: bool,
    stats_delta: Value,
) -> Value {
    let summary = serde_json::from_str::<Value>(&entry.summary_json).unwrap_or_else(|_| {
        json!({
            "raw": entry.summary_json
        })
    });
    success_envelope_with_stats(
        json!({
            "available": true,
            "entity": entity_json(entity),
            "summary": summary,
            "cache": {
                "hit": cache_hit,
                "prompt_template_id": entry.key.prompt_template_id,
                "model_id": entry.key.model_tier,
                "guidance_fingerprint": entry.key.guidance_fingerprint,
                "stale_semantic": stale_semantic,
                "created_at": entry.created_at,
                "last_accessed_at": entry.last_accessed_at
            },
            "usage": {
                "cost_usd": entry.cost_usd,
                "tokens_input": entry.tokens_input,
                "tokens_output": entry.tokens_output
            }
        }),
        stats_delta,
    )
}

fn tool_json_rpc_response(id: &Value, envelope: &Value) -> Value {
    let is_error = !envelope
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or_default();
    result_response(
        id,
        &json!({
            "content": [
                {
                    "type": "text",
                    "text": serde_json::to_string(&envelope).expect("tool envelope serializes")
                }
            ],
            "isError": is_error
        }),
    )
}

fn entity_json(entity: &EntityRow) -> Value {
    json!({
        "id": entity.id,
        "kind": entity.kind,
        "name": entity.name,
        "short_name": entity.short_name,
        "source_file_path": entity.source_file_path,
        "source_line_start": entity.source_line_start,
        "source_line_end": entity.source_line_end,
        "content_hash": entity.content_hash
    })
}

fn source_excerpt(entity: &EntityRow) -> String {
    entity
        .source_file_path
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|source| {
            if source.len() > 8_000 {
                source.chars().take(8_000).collect()
            } else {
                source
            }
        })
        .unwrap_or_default()
}

fn unresolved_sites_json(sites: &[UnresolvedCallSiteRow]) -> String {
    serde_json::to_string(
        &sites
            .iter()
            .map(|site| {
                json!({
                    "caller_entity_id": site.caller_entity_id,
                    "caller_content_hash": site.caller_content_hash,
                    "site_key": site.site_key,
                    "site_ordinal": site.site_ordinal,
                    "source_file_id": site.source_file_id,
                    "source_byte_start": site.source_byte_start,
                    "source_byte_end": site.source_byte_end,
                    "callee_expr": site.callee_expr
                })
            })
            .collect::<Vec<_>>(),
    )
    .expect("unresolved site JSON serializes")
}

fn entities_json(entities: &[EntityRow]) -> String {
    serde_json::to_string(&entities.iter().map(entity_json).collect::<Vec<_>>())
        .expect("candidate entity JSON serializes")
}

fn inferred_records_from_result(
    read: &InferredRead,
    result_json: &str,
    max_edges: usize,
) -> Result<Vec<InferredCallEdgeRecord>, InferredDispatchFailure> {
    let parsed: InferredCallsResponse = serde_json::from_str(result_json).map_err(|err| {
        InferredDispatchFailure::new(
            "llm-invalid-json",
            &format!("inferred provider returned invalid JSON: {err}"),
            true,
        )
    })?;
    let cache_key = inferred_edge_cache_key_id(&read.key);
    let sites_by_key = read
        .sites
        .iter()
        .map(|site| (site.site_key.as_str(), site))
        .collect::<HashMap<_, _>>();
    let mut records = Vec::new();
    for edge in parsed.edges.into_iter().take(max_edges) {
        if edge.target_id.trim().is_empty() {
            continue;
        }
        let site = match edge.site_key.as_deref() {
            Some(site_key) => sites_by_key.get(site_key).copied(),
            None if read.sites.len() == 1 => read.sites.first(),
            None => None,
        };
        let Some(site) = site else {
            continue;
        };
        let properties = json!({
            "model_id": read.key.model_id,
            "prompt_version": read.key.prompt_version,
            "caller_content_hash": read.key.caller_content_hash,
            "inference_cache_key": cache_key,
            "site_key": site.site_key,
            "model_confidence": edge.confidence,
            "rationale": edge.rationale
        });
        records.push(InferredCallEdgeRecord {
            from_id: read.caller.id.clone(),
            to_id: edge.target_id,
            source_file_id: site.source_file_id.clone(),
            source_byte_start: site.source_byte_start,
            source_byte_end: site.source_byte_end,
            properties_json: properties.to_string(),
        });
    }
    Ok(records)
}

fn stale_semantic(entry: &SummaryCacheEntry, caller_count: i64, fan_out: i64) -> bool {
    entry.stale_semantic
        || count_drifted(entry.caller_count, caller_count)
        || count_drifted(entry.fan_out, fan_out)
}

fn count_drifted(stored: i64, current: i64) -> bool {
    if stored == current {
        return false;
    }
    if stored == 0 {
        return current != 0;
    }
    i128::from((current - stored).abs()) * 2 > i128::from(stored.abs())
}

fn summary_cache_expired(created_at: &str, now: &str, max_age_days: u32) -> bool {
    let Some(created) = timestamp_day_index(created_at) else {
        return false;
    };
    let Some(current) = timestamp_day_index(now) else {
        return false;
    };
    current.saturating_sub(created) > i64::from(max_age_days)
}

fn timestamp_day_index(raw: &str) -> Option<i64> {
    if let Some(seconds) = raw.strip_prefix("unix:") {
        return seconds.parse::<i64>().ok().map(|value| value / 86_400);
    }
    let date = raw.get(..10)?;
    let mut parts = date.split('-');
    let year = parts.next()?.parse::<i64>().ok()?;
    let month = parts.next()?.parse::<i64>().ok()?;
    let day = parts.next()?.parse::<i64>().ok()?;
    Some(days_from_civil(year, month, day))
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * month_prime + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn default_now_string() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    format!("unix:{seconds}")
}

fn caller_json(
    conn: &rusqlite::Connection,
    edge: &CallEdgeMatch,
) -> Result<Option<Value>, StorageError> {
    Ok(entity_by_id(conn, &edge.from_id)?.map(|entity| {
        json!({
            "entity": entity_json(&entity),
            "edge_confidence": edge.confidence.as_str(),
            "source_byte_start": edge.source_byte_start,
            "source_byte_end": edge.source_byte_end,
            "target_id": edge.to_id,
            "stored_to_id": edge.stored_to_id
        })
    }))
}

fn callee_json(
    conn: &rusqlite::Connection,
    edge: &CallEdgeMatch,
) -> Result<Option<Value>, StorageError> {
    Ok(entity_by_id(conn, &edge.to_id)?.map(|entity| {
        json!({
            "entity": entity_json(&entity),
            "edge_confidence": edge.confidence.as_str(),
            "source_byte_start": edge.source_byte_start,
            "source_byte_end": edge.source_byte_end,
            "stored_to_id": edge.stored_to_id
        })
    }))
}

fn path_json(conn: &rusqlite::Connection, path: &[String]) -> Result<Value, StorageError> {
    let entities = path
        .iter()
        .filter_map(|entity_id| entity_by_id(conn, entity_id).transpose())
        .map(|row| row.map(|entity| entity_json(&entity)))
        .collect::<Result<Vec<_>, StorageError>>()?;
    Ok(Value::Array(entities))
}

fn reference_neighbors(
    conn: &rusqlite::Connection,
    entity_id: &str,
    direction: ReferenceDirection,
) -> Result<Vec<Value>, StorageError> {
    let (predicate, neighbor_column) = match direction {
        ReferenceDirection::In => ("to_id = ?1", "from_id"),
        ReferenceDirection::Out => ("from_id = ?1", "to_id"),
    };
    let sql = format!(
        "SELECT {neighbor_column}, confidence, source_byte_start, source_byte_end \
         FROM edges \
         WHERE kind = 'references' AND {predicate} \
         ORDER BY {neighbor_column}, source_byte_start, source_byte_end"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![entity_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
            row.get::<_, Option<i64>>(3)?,
        ))
    })?;
    let mut neighbors = Vec::new();
    for row in rows {
        let (neighbor_id, confidence, source_byte_start, source_byte_end) = row?;
        if let Some(entity) = entity_by_id(conn, &neighbor_id)? {
            neighbors.push(json!({
                "entity": entity_json(&entity),
                "edge_confidence": confidence,
                "source_byte_start": source_byte_start,
                "source_byte_end": source_byte_end
            }));
        }
    }
    Ok(neighbors)
}

fn result_response(id: &Value, result: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

#[cfg(test)]
mod tests {
    use super::list_tools;

    #[test]
    fn tools_list_exposes_exact_docstrings() {
        let tools = list_tools();

        assert_eq!(tools.len(), 7);
        assert_eq!(tools[0].name, "entity_at");
        assert_eq!(
            tools[0].description,
            "Return the innermost Clarion entity whose source range contains a file and line. Paths are normalized relative to the project root. Returns no match rather than guessing when ranges are absent."
        );
        assert_eq!(tools[1].name, "find_entity");
        assert_eq!(
            tools[1].description,
            "Search Clarion entities by id, name, short name, and summary text stored on entity rows. Results are paginated and ranked by FTS match where possible. This does not traverse the graph and does not search on-demand summary_cache entries."
        );
        assert_eq!(tools[2].name, "callers_of");
        assert_eq!(
            tools[2].description,
            "Return entities that call the given entity. Default confidence is resolved, so ambiguous static candidates and LLM-inferred edges are excluded unless explicitly requested. Ambiguous edges expand all candidates; inferred edges may trigger bounded LLM dispatch."
        );
        assert_eq!(tools[3].name, "execution_paths_from");
        assert_eq!(
            tools[3].description,
            "Return bounded calls-only execution paths starting at an entity. Default confidence is resolved. max_depth defaults to 3 and traversal also stops at the server edge cap; responses say when they are truncated."
        );
        assert_eq!(tools[4].name, "summary");
        assert_eq!(
            tools[4].description,
            "Return an on-demand cached summary for one entity. In v0.1 this is leaf scope only: module summaries describe the module docstring and top-level members, not an aggregation of contained function/class summaries."
        );
        assert_eq!(tools[5].name, "issues_for");
        assert_eq!(
            tools[5].description,
            "Return Filigree issues attached to this Clarion entity, optionally including issues attached to contained entities. Filigree is an enrichment source; if unavailable, the tool returns an unavailable envelope instead of failing Clarion."
        );
        assert_eq!(tools[6].name, "neighborhood");
        assert_eq!(
            tools[6].description,
            "Return the one-hop Clarion neighborhood around an entity: callers, callees, container, contained entities, and references. Default confidence is resolved; ambiguous and inferred calls are opt-in. References are not execution flow."
        );
    }

    #[test]
    fn initialize_returns_server_info_and_tools_capability() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "0.0.0"}
            }
        }));

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert_eq!(
            response["result"]["protocolVersion"],
            super::MCP_PROTOCOL_VERSION
        );
        assert_eq!(response["result"]["serverInfo"]["name"], "clarion");
        assert!(response["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn tools_list_request_wraps_all_tools() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": "tools-1",
            "method": "tools/list",
            "params": {}
        }));

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], "tools-1");
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 7);
        assert_eq!(response["result"]["tools"][0]["name"], "entity_at");
    }

    #[test]
    fn unknown_method_is_json_rpc_method_not_found() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "not/real",
            "params": {}
        }));

        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn call_tool_rejects_unknown_tool() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {"name": "not_a_tool", "arguments": {}}
        }));

        assert_eq!(response["error"]["code"], -32601);
        assert_eq!(response["error"]["message"], "unknown tool: not_a_tool");
    }

    #[test]
    fn call_tool_rejects_invalid_params() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {"arguments": {}}
        }));

        assert_eq!(response["error"]["code"], -32602);
    }

    #[test]
    fn frame_dispatch_decodes_and_reencodes_json_rpc() {
        let frame = clarion_core::plugin::Frame {
            body: serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "tools/list",
                "params": {}
            }))
            .unwrap(),
        };

        let response = super::handle_frame(&frame).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&response.body).unwrap();

        assert_eq!(decoded["jsonrpc"], "2.0");
        assert_eq!(decoded["id"], 10);
        assert_eq!(decoded["result"]["tools"].as_array().unwrap().len(), 7);
    }

    #[test]
    fn serve_stdio_handles_multiple_content_length_frames() {
        let mut input = Vec::new();
        clarion_core::plugin::write_frame(
            &mut input,
            &clarion_core::plugin::Frame {
                body: serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 11,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "test-client", "version": "0.0.0"}
                    }
                }))
                .unwrap(),
            },
        )
        .unwrap();
        clarion_core::plugin::write_frame(
            &mut input,
            &clarion_core::plugin::Frame {
                body: serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 12,
                    "method": "tools/list",
                    "params": {}
                }))
                .unwrap(),
            },
        )
        .unwrap();

        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
        let mut output = Vec::new();

        super::serve_stdio(&mut reader, &mut output).unwrap();

        let mut response_reader = std::io::BufReader::new(std::io::Cursor::new(output));
        let first = clarion_core::plugin::read_frame(
            &mut response_reader,
            clarion_core::plugin::ContentLengthCeiling::new(usize::MAX),
        )
        .unwrap();
        let second = clarion_core::plugin::read_frame(
            &mut response_reader,
            clarion_core::plugin::ContentLengthCeiling::new(usize::MAX),
        )
        .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
        let second_json: serde_json::Value = serde_json::from_slice(&second.body).unwrap();

        assert_eq!(first_json["id"], 11);
        assert_eq!(first_json["result"]["serverInfo"]["name"], "clarion");
        assert_eq!(second_json["id"], 12);
        assert_eq!(second_json["result"]["tools"].as_array().unwrap().len(), 7);
    }
}
