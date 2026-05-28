//! MCP protocol surface for Clarion.

pub mod config;
pub mod filigree;
pub mod snapshot;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clarion_core::{
    EdgeConfidence, INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput,
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID, LeafSummaryPromptInput, LlmProvider, LlmProviderError,
    LlmPurpose, LlmRequest, LlmResponse, build_inferred_calls_prompt, build_leaf_summary_prompt,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use time::{Date, Month, OffsetDateTime, macros::format_description};
use tokio::sync::{Mutex as AsyncMutex, broadcast, mpsc, oneshot};

use clarion_core::plugin::{ContentLengthCeiling, Frame, TransportError};
use clarion_storage::{
    CallEdgeMatch, EntityRow, InferredCallEdgeRecord, InferredEdgeCacheEntry, InferredEdgeCacheKey,
    InferredEdgeWriteStats, ReaderPool, ReferenceDirection, StorageError, SummaryCacheEntry,
    SummaryCacheKey, UnresolvedCallSiteRow, WriterCmd, call_edges_from, call_edges_targeting,
    candidate_entities_for_unresolved_sites, child_entity_ids, contained_entity_ids,
    entity_at_line, entity_by_id, existing_entity_ids, find_entities, inferred_edge_cache_key_id,
    inferred_edge_cache_lookup, normalize_source_path, reference_edges_for_entity,
    subsystem_members, summary_cache_lookup, unresolved_call_sites_for_caller,
    unresolved_callers_for_target,
};

use crate::config::LlmConfig;
use crate::filigree::{EntityAssociation, EntityAssociationsResponse, FiligreeLookup};

/// MCP protocol revision supported by the B.6 stdio server.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const EMPTY_GUIDANCE_FINGERPRINT: &str = "guidance-empty";

type InferredInflight =
    Arc<AsyncMutex<HashMap<InferredEdgeCacheKey, broadcast::Sender<InferredDispatchOutcome>>>>;

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
        ToolDefinition {
            name: "subsystem_members",
            description: "List module entities assigned to a subsystem entity.",
            input_schema: id_schema(),
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

/// Handle state-free MCP requests such as `initialize` and `tools/list`.
///
/// Storage-backed tool calls require [`ServerState::handle_json_rpc`].
#[must_use]
pub fn handle_json_rpc(request: &Value) -> Option<Value> {
    if is_json_rpc_notification(request) {
        return None;
    }
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return Some(error_response(&id, -32600, "invalid request"));
    };

    Some(match method {
        "initialize" => result_response(&id, &initialize_result()),
        "tools/list" => result_response(&id, &json!({"tools": list_tools()})),
        "tools/call" => error_response(
            &id,
            -32601,
            "tools/call requires ServerState::handle_json_rpc",
        ),
        _ => error_response(&id, -32601, "method not found"),
    })
}

pub struct ServerState {
    project_root: PathBuf,
    readers: ReaderPool,
    execution_edge_cap: usize,
    summary_llm: Option<SummaryLlmState>,
    clock: Arc<dyn Fn() -> String + Send + Sync>,
    budget: Arc<Mutex<BudgetLedger>>,
    inferred_inflight: InferredInflight,
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

    pub async fn handle_json_rpc(&self, request: &Value) -> Option<Value> {
        if is_json_rpc_notification(request) {
            return None;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            return Some(error_response(&id, -32600, "invalid request"));
        };

        Some(match method {
            "initialize" => result_response(&id, &initialize_result()),
            "tools/list" => result_response(&id, &json!({"tools": list_tools()})),
            "tools/call" => self.handle_tool_call(&id, request.get("params")).await,
            _ => error_response(&id, -32601, "method not found"),
        })
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
            "subsystem_members" => match self.tool_subsystem_members(arguments).await {
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
            Err(err) => {
                return tool_error_envelope(
                    "storage-error",
                    &err.to_string(),
                    storage_retryable(&err),
                );
            }
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
                Err(err) => {
                    return tool_error_envelope(
                        "storage-error",
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
            Err(err) => {
                tool_error_envelope("storage-error", &err.to_string(), storage_retryable(&err))
            }
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
            Err(err) => {
                return Ok(tool_error_envelope(
                    "storage-error",
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
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

    async fn tool_subsystem_members(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let subsystem_id = required_str(arguments, "id")?.to_owned();
        let result = self
            .readers
            .with_reader(move |conn| {
                let Some(subsystem) = entity_by_id(conn, &subsystem_id)? else {
                    return Ok(tool_error_envelope(
                        "entity-not-found",
                        &format!("entity {subsystem_id} was not found"),
                        false,
                    ));
                };
                if subsystem.kind != "subsystem" {
                    return Ok(tool_error_envelope(
                        "not-a-subsystem",
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
            Err(err) => {
                return Ok(tool_error_envelope(
                    "storage-error",
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };

        let SummaryRead::Ready(ready) = read else {
            return Ok(summary_read_error(read));
        };

        if let Some(envelope) = self.cached_summary_envelope(&ready, &now).await {
            return Ok(envelope);
        }

        if self.summary_budget_blocked() {
            return Ok(token_ceiling_envelope(
                "LLM session token ceiling has been reached",
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

        if let Some(reason) = briefing_block_reason(&read.caller) {
            tracing::warn!(
                caller_id = %caller_id,
                briefing_blocked = %reason,
                "skipping inferred-edge dispatch for briefing-blocked caller"
            );
            return Ok(InferredDispatchStats::briefing_blocked());
        }

        if let Some(cached) = read.cached.clone() {
            return self.materialize_cached_inferred(read, cached).await;
        }

        if self.summary_budget_blocked() {
            return Err(InferredDispatchFailure::new(
                "token-ceiling-exceeded",
                "LLM session token ceiling has been reached",
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
        let (edges, dropped) = self.drop_unresolved_inferred_targets(edges).await?;
        let write = self
            .send_writer(&llm.writer, |ack| WriterCmd::InsertInferredEdges {
                cache_entry: Box::new(cached),
                edges,
                ack,
            })
            .await
            .map_err(|err| InferredDispatchFailure::from_storage(&err))?;
        let mut stats = InferredDispatchStats::cache_hit(write);
        stats.unresolved_targets_dropped_total = dropped;
        Ok(stats)
    }

    async fn coalesced_inferred_dispatch(
        &self,
        key: InferredEdgeCacheKey,
        read: InferredRead,
        llm: InferenceLlmState,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let (maybe_rx, leader_sender) = {
            let mut in_flight = self.inferred_inflight.lock().await;
            if let Some(sender) = in_flight.get(&key) {
                (Some(sender.subscribe()), None)
            } else {
                let (sender, _) = broadcast::channel(8);
                in_flight.insert(key.clone(), sender.clone());
                (None, Some(sender))
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

        let guard = InferredInflightGuard::new(
            Arc::clone(&self.inferred_inflight),
            key,
            leader_sender.expect("leader sender is present for non-coalesced dispatch"),
        );
        let outcome =
            InferredDispatchOutcome::from_result(self.perform_inferred_dispatch(read, &llm).await);
        if let Some(sender) = guard.remove().await {
            let _ = sender.send(outcome.clone());
        }
        outcome.into_result()
    }

    async fn perform_inferred_dispatch(
        &self,
        read: InferredRead,
        llm: &InferenceLlmState,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let caller_source_excerpt =
            verified_source_excerpt(&read.caller).map_err(|err| err.to_inferred_failure())?;
        let prompt = build_inferred_calls_prompt(&InferredCallsPromptInput {
            caller_entity_id: read.caller.id.clone(),
            caller_source_excerpt,
            unresolved_call_sites_json: unresolved_sites_json(&read.sites),
            candidate_entities_json: entities_json(&read.candidates),
            max_edges: self.max_inferred_edges_per_caller(),
        });
        let request = LlmRequest {
            purpose: LlmPurpose::InferredEdges,
            model_id: read.key.model_id.clone(),
            prompt_id: prompt.id.to_owned(),
            prompt: prompt.body,
            max_output_tokens: 2048,
        };
        let Some(reservation) = self.reserve_budget(
            llm.provider.estimate_tokens(&request),
            llm.config.session_token_ceiling,
        ) else {
            return Err(InferredDispatchFailure::new(
                "token-ceiling-exceeded",
                "LLM session token ceiling has been reached",
                false,
            ));
        };
        let response = invoke_llm_provider(Arc::clone(&llm.provider), request)
            .await
            .map_err(|err| {
                InferredDispatchFailure::new(
                    "llm-provider-error",
                    &err.to_string(),
                    err.retryable(),
                )
            })?;
        if !reservation.commit(
            u64::from(response.total_tokens),
            llm.config.session_token_ceiling,
        ) {
            return Err(InferredDispatchFailure::new(
                "token-ceiling-exceeded",
                "LLM session token ceiling has been reached",
                false,
            ));
        }
        let edges = match inferred_records_from_result(
            &read,
            &response.output_json,
            self.max_inferred_edges_per_caller(),
        ) {
            Ok(edges) => edges,
            Err(err) if err.code == "llm-invalid-json" => {
                let message = err.message.clone();
                return Err(err.with_stats(
                    inferred_usage_stats(&response, true),
                    vec![json!({
                        "code": "CLA-LLM-INVALID-JSON",
                        "message": message,
                        "usage": llm_usage_json(&response)
                    })],
                ));
            }
            Err(err) => return Err(err),
        };
        let (edges, dropped) = self.drop_unresolved_inferred_targets(edges).await?;
        let now = (self.clock)();
        let entry = InferredEdgeCacheEntry {
            key: read.key,
            result_json: response.output_json.clone(),
            cost_usd: response.cost_usd,
            token_count: i64::from(response.total_tokens),
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
        let mut stats = InferredDispatchStats::cache_miss(write, &response);
        stats.unresolved_targets_dropped_total = dropped;
        Ok(stats)
    }

    /// Strip `to_id`s that don't exist in the `entities` table so the
    /// writer-actor's FK-protected INSERT never sees a hallucinated edge
    /// target (clarion-df58379de4). Returns the surviving records and the
    /// count of dropped edges so callers can fold the number into
    /// `InferredDispatchStats`.
    async fn drop_unresolved_inferred_targets(
        &self,
        records: Vec<InferredCallEdgeRecord>,
    ) -> Result<(Vec<InferredCallEdgeRecord>, u64), InferredDispatchFailure> {
        if records.is_empty() {
            return Ok((records, 0));
        }
        let unique_targets: Vec<String> = records
            .iter()
            .map(|record| record.to_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let existing = self
            .readers
            .with_reader({
                let targets = unique_targets.clone();
                move |conn| existing_entity_ids(conn, &targets)
            })
            .await
            .map_err(|err| InferredDispatchFailure::from_storage(&err))?;
        let original_len = records.len();
        let kept: Vec<InferredCallEdgeRecord> = records
            .into_iter()
            .filter(|record| existing.contains(&record.to_id))
            .collect();
        let dropped = u64::try_from(original_len - kept.len()).unwrap_or(0);
        Ok((kept, dropped))
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
                if entity.kind == "subsystem" {
                    return Ok(SummaryRead::ScopeDeferred(Box::new(entity)));
                }
                if let Some(reason) = briefing_block_reason(&entity) {
                    return Ok(SummaryRead::BriefingBlocked(Box::new(entity), reason));
                }
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
            return Some(tool_error_envelope(
                "storage-error",
                &err.to_string(),
                storage_retryable(&err),
            ));
        }
        Some(summary_success_envelope(
            &ready.entity,
            cached,
            true,
            stale_semantic(cached, ready.caller_count, ready.fan_out),
            None,
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
        let source_excerpt = match verified_source_excerpt(&ready.entity) {
            Ok(excerpt) => excerpt,
            Err(err) => return err.to_envelope(),
        };
        let prompt = build_leaf_summary_prompt(&LeafSummaryPromptInput {
            entity_id: ready.entity.id.clone(),
            kind: ready.entity.kind.clone(),
            name: ready.entity.name.clone(),
            source_excerpt,
        });
        let request = LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: model_id.clone(),
            prompt_id: prompt.id.to_owned(),
            prompt: prompt.body,
            max_output_tokens: 512,
        };
        let Some(reservation) = self.reserve_budget(
            summary_llm.provider.estimate_tokens(&request),
            summary_llm.config.session_token_ceiling,
        ) else {
            return token_ceiling_envelope("LLM session token ceiling has been reached");
        };
        let response = match invoke_llm_provider(Arc::clone(&summary_llm.provider), request).await {
            Ok(response) => response,
            Err(err) => {
                return tool_error_envelope(
                    "llm-provider-error",
                    &err.to_string(),
                    err.retryable(),
                );
            }
        };

        if !reservation.commit(
            u64::from(response.total_tokens),
            summary_llm.config.session_token_ceiling,
        ) {
            return token_ceiling_envelope("LLM session token ceiling has been reached");
        }

        if serde_json::from_str::<Value>(&response.output_json).is_err() {
            return tool_error_envelope_with_diagnostics(
                "llm-invalid-json",
                "summary provider returned non-JSON output",
                true,
                summary_usage_stats(&response, true),
                vec![json!({
                    "code": "CLA-LLM-INVALID-JSON",
                    "message": "summary provider returned non-JSON output",
                    "usage": llm_usage_json(&response)
                })],
            );
        }

        let cached_input_tokens = i64::from(response.cached_input_tokens);
        let stats_delta = summary_usage_stats(&response, false);
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
            return tool_error_envelope("storage-error", &err.to_string(), storage_retryable(&err));
        }

        summary_success_envelope(
            &ready.entity,
            &entry,
            false,
            false,
            Some(cached_input_tokens),
            stats_delta,
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

    fn reserve_budget(
        &self,
        estimate_tokens: u64,
        ceiling_tokens: u64,
    ) -> Option<BudgetReservation> {
        let mut budget = self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if budget.blocked
            || budget
                .spent_tokens
                .saturating_add(budget.reserved_tokens)
                .saturating_add(estimate_tokens)
                > ceiling_tokens
        {
            budget.blocked = true;
            return None;
        }
        budget.reserved_tokens = budget.reserved_tokens.saturating_add(estimate_tokens);
        Some(BudgetReservation {
            budget: Arc::clone(&self.budget),
            amount_tokens: estimate_tokens,
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
            || "anthropic/claude-sonnet-4.6".to_owned(),
            |summary| {
                summary
                    .provider
                    .tier_to_model("summary")
                    .unwrap_or(&summary.config.model_id)
                    .to_owned()
            },
        )
    }

    fn inferred_edges_model_id(&self) -> String {
        self.summary_llm.as_ref().map_or_else(
            || "anthropic/claude-sonnet-4.6".to_owned(),
            |summary| {
                summary
                    .provider
                    .tier_to_model("inferred_edges")
                    .unwrap_or(&summary.config.model_id)
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

async fn invoke_llm_provider(
    provider: Arc<dyn LlmProvider>,
    request: LlmRequest,
) -> Result<LlmResponse, LlmProviderError> {
    tokio::task::spawn_blocking(move || provider.invoke(request))
        .await
        .map_err(|err| LlmProviderError::InvalidResponse {
            message: format!("LLM provider task failed: {err}"),
            retryable: true,
        })?
}

struct SummaryLlmState {
    writer: mpsc::Sender<WriterCmd>,
    config: LlmConfig,
    provider: Arc<dyn LlmProvider>,
}

#[derive(Default)]
struct BudgetLedger {
    spent_tokens: u64,
    reserved_tokens: u64,
    blocked: bool,
}

struct BudgetReservation {
    budget: Arc<Mutex<BudgetLedger>>,
    amount_tokens: u64,
    active: bool,
}

impl BudgetReservation {
    fn commit(mut self, actual_tokens: u64, ceiling_tokens: u64) -> bool {
        let mut budget = self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.active {
            budget.reserved_tokens = budget.reserved_tokens.saturating_sub(self.amount_tokens);
            self.active = false;
        }
        // `budget.blocked` gates *new* reservations, not in-flight commits.
        // A reservation that already cleared reserve_budget paid for its
        // dispatch slot; commit it iff the actual usage fits the ceiling.
        if budget.spent_tokens.saturating_add(actual_tokens) > ceiling_tokens {
            budget.blocked = true;
            return false;
        }
        budget.spent_tokens = budget.spent_tokens.saturating_add(actual_tokens);
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
        budget.reserved_tokens = budget.reserved_tokens.saturating_sub(self.amount_tokens);
        self.active = false;
    }
}

enum SummaryRead {
    Ready(Box<SummaryReady>),
    EntityNotFound(String),
    MissingContentHash(String),
    ScopeDeferred(Box<EntityRow>),
    BriefingBlocked(Box<EntityRow>, String),
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

struct InferredInflightGuard {
    in_flight: InferredInflight,
    key: InferredEdgeCacheKey,
    sender: broadcast::Sender<InferredDispatchOutcome>,
    active: bool,
}

impl InferredInflightGuard {
    fn new(
        in_flight: InferredInflight,
        key: InferredEdgeCacheKey,
        sender: broadcast::Sender<InferredDispatchOutcome>,
    ) -> Self {
        Self {
            in_flight,
            key,
            sender,
            active: true,
        }
    }

    async fn remove(mut self) -> Option<broadcast::Sender<InferredDispatchOutcome>> {
        let removed = remove_matching_inferred_inflight(
            Arc::clone(&self.in_flight),
            self.key.clone(),
            self.sender.clone(),
        )
        .await;
        self.active = false;
        removed
    }
}

impl Drop for InferredInflightGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let in_flight = Arc::clone(&self.in_flight);
        let key = self.key.clone();
        let sender = self.sender.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = remove_matching_inferred_inflight(in_flight, key, sender).await;
            });
        }
    }
}

async fn remove_matching_inferred_inflight(
    in_flight: InferredInflight,
    key: InferredEdgeCacheKey,
    sender: broadcast::Sender<InferredDispatchOutcome>,
) -> Option<broadcast::Sender<InferredDispatchOutcome>> {
    let mut map = in_flight.lock().await;
    if map
        .get(&key)
        .is_some_and(|current| current.same_channel(&sender))
    {
        map.remove(&key)
    } else {
        None
    }
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
    /// LLM-proposed `to_id` values that did not resolve in the `entities`
    /// table at write time (clarion-df58379de4). Counted here, dropped from
    /// the persisted edge set to avoid the FK violation that previously
    /// poisoned the cache row and re-burned LLM tokens on retry.
    unresolved_targets_dropped_total: u64,
    briefing_blocked_total: u64,
    candidate_callers_considered: u64,
    coalesced_waits_total: u64,
    tokens_input: i64,
    tokens_cached_input: i64,
    tokens_output: i64,
    tokens_total: i64,
    cost_usd: f64,
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

    fn cache_miss(write: InferredEdgeWriteStats, response: &LlmResponse) -> Self {
        Self {
            cache_misses_total: 1,
            edges_materialized_total: write.inserted_edges,
            edges_skipped_static_duplicates_total: write.skipped_static_duplicates,
            tokens_input: i64::from(response.input_tokens),
            tokens_cached_input: i64::from(response.cached_input_tokens),
            tokens_output: i64::from(response.output_tokens),
            tokens_total: i64::from(response.total_tokens),
            cost_usd: response.cost_usd,
            ..Self::default()
        }
    }

    fn briefing_blocked() -> Self {
        Self {
            briefing_blocked_total: 1,
            ..Self::default()
        }
    }

    fn merge(&mut self, other: &Self) {
        self.cache_hits_total += other.cache_hits_total;
        self.cache_misses_total += other.cache_misses_total;
        self.edges_materialized_total += other.edges_materialized_total;
        self.edges_skipped_static_duplicates_total += other.edges_skipped_static_duplicates_total;
        self.unresolved_targets_dropped_total += other.unresolved_targets_dropped_total;
        self.briefing_blocked_total += other.briefing_blocked_total;
        self.candidate_callers_considered += other.candidate_callers_considered;
        self.coalesced_waits_total += other.coalesced_waits_total;
        self.tokens_input += other.tokens_input;
        self.tokens_cached_input += other.tokens_cached_input;
        self.tokens_output += other.tokens_output;
        self.tokens_total += other.tokens_total;
        self.cost_usd += other.cost_usd;
    }

    fn to_json(&self) -> Value {
        json!({
            "inferred_dispatch_cache_hits_total": self.cache_hits_total,
            "inferred_dispatch_misses_total": self.cache_misses_total,
            "inferred_edges_materialized_total": self.edges_materialized_total,
            "inferred_edges_skipped_static_duplicates_total": self.edges_skipped_static_duplicates_total,
            "inferred_unresolved_targets_dropped_total": self.unresolved_targets_dropped_total,
            "inferred_dispatch_briefing_blocked_total": self.briefing_blocked_total,
            "inferred_candidate_callers_considered": self.candidate_callers_considered,
            "inferred_dispatch_coalesced_total": self.coalesced_waits_total,
            "inferred_tokens_input": self.tokens_input,
            "inferred_tokens_cached_input": self.tokens_cached_input,
            "inferred_tokens_output": self.tokens_output,
            "inferred_tokens_total": self.tokens_total,
            "inferred_cost_usd": self.cost_usd
        })
    }
}

#[derive(Debug, Clone)]
struct InferredDispatchFailure {
    code: &'static str,
    message: String,
    retryable: bool,
    stats_delta: Value,
    diagnostics: Vec<Value>,
}

impl InferredDispatchFailure {
    fn new(code: &'static str, message: &str, retryable: bool) -> Self {
        Self {
            code,
            message: message.to_owned(),
            retryable,
            stats_delta: json!({}),
            diagnostics: Vec::new(),
        }
    }

    fn from_storage(err: &StorageError) -> Self {
        // FK violations are deterministic against the same row set; treating
        // them as `retryable=true` causes the client to re-issue the LLM call
        // and re-pay the token cost (clarion-df58379de4). Mark them
        // non-retryable so a client honouring the hint gives up immediately.
        Self {
            code: "storage-error",
            message: err.to_string(),
            retryable: !err.is_foreign_key_violation(),
            stats_delta: json!({}),
            diagnostics: Vec::new(),
        }
    }

    fn with_stats(mut self, stats_delta: Value, diagnostics: Vec<Value>) -> Self {
        self.stats_delta = stats_delta;
        self.diagnostics = diagnostics;
        self
    }

    fn to_envelope(&self) -> Value {
        if self.code == "token-ceiling-exceeded" {
            return token_ceiling_envelope(&self.message);
        }
        tool_error_envelope_with_diagnostics(
            self.code,
            &self.message,
            self.retryable,
            self.stats_delta.clone(),
            self.diagnostics.clone(),
        )
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

/// Decode and handle a state-free MCP frame.
///
/// Storage-backed tool calls require [`handle_frame_with_state`].
pub fn handle_frame(frame: &Frame) -> Result<Option<Frame>, McpError> {
    let request = serde_json::from_slice(&frame.body)?;
    let Some(response) = handle_json_rpc(&request) else {
        return Ok(None);
    };
    Ok(Some(encode_response_frame(&response)?))
}

pub async fn handle_frame_with_state(
    state: &ServerState,
    frame: &Frame,
) -> Result<Option<Frame>, McpError> {
    let request = serde_json::from_slice(&frame.body)?;
    let Some(response) = state.handle_json_rpc(&request).await else {
        return Ok(None);
    };
    Ok(Some(encode_response_frame(&response)?))
}

fn handle_stdio_frame(frame: &Frame) -> Result<Option<Frame>, McpError> {
    handle_frame(frame)
}

async fn handle_stdio_frame_with_state(
    state: &ServerState,
    frame: &Frame,
) -> Result<Option<Frame>, McpError> {
    handle_frame_with_state(state, frame).await
}

fn encode_response_frame(response: &Value) -> Result<Frame, McpError> {
    Ok(Frame {
        body: serde_json::to_vec(response)?,
    })
}

fn is_json_rpc_notification(request: &Value) -> bool {
    request
        .as_object()
        .is_some_and(|object| object.get("method").is_some() && object.get("id").is_none())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdioFraming {
    ContentLength,
    JsonLine,
}

struct StdioFrame {
    body: Vec<u8>,
    framing: StdioFraming,
}

fn read_stdio_frame(reader: &mut impl std::io::BufRead) -> Result<Option<StdioFrame>, McpError> {
    let Some(first_byte) = peek_stdio_frame_start(reader)? else {
        return Ok(None);
    };
    if first_byte == b'{' || first_byte == b'[' || first_byte.is_ascii_whitespace() {
        return Ok(Some(read_json_line_frame(reader)?));
    }
    match clarion_core::plugin::read_frame(reader, ContentLengthCeiling::DEFAULT) {
        Ok(frame) => Ok(Some(StdioFrame {
            body: frame.body,
            framing: StdioFraming::ContentLength,
        })),
        Err(TransportError::Io(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn peek_stdio_frame_start(reader: &mut impl std::io::BufRead) -> Result<Option<u8>, McpError> {
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(None);
        }
        let blank_prefix = buffer
            .iter()
            .take_while(|byte| matches!(byte, b'\r' | b'\n'))
            .count();
        if blank_prefix == 0 {
            return Ok(Some(buffer[0]));
        }
        reader.consume(blank_prefix);
    }
}

fn read_json_line_frame(reader: &mut impl std::io::BufRead) -> Result<StdioFrame, McpError> {
    let mut body = Vec::new();
    let read = std::io::BufRead::read_until(reader, b'\n', &mut body)?;
    if read == 0 {
        return Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "EOF while reading MCP JSON line",
        ))
        .into());
    }
    while matches!(body.last(), Some(b'\n' | b'\r')) {
        body.pop();
    }
    Ok(StdioFrame {
        body,
        framing: StdioFraming::JsonLine,
    })
}

fn write_stdio_response(
    writer: &mut impl std::io::Write,
    response: &Frame,
    framing: StdioFraming,
) -> Result<(), McpError> {
    match framing {
        StdioFraming::ContentLength => {
            clarion_core::plugin::write_frame(writer, response)?;
        }
        StdioFraming::JsonLine => {
            writer.write_all(&response.body)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// Serve state-free MCP protocol metadata over stdio.
///
/// Storage-backed tool calls require [`serve_stdio_with_state`].
pub fn serve_stdio(
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
) -> Result<(), McpError> {
    loop {
        let Some(frame) = read_stdio_frame(reader)? else {
            return Ok(());
        };
        let framing = frame.framing;
        if let Some(response) = handle_stdio_frame(&Frame { body: frame.body })? {
            write_stdio_response(writer, &response, framing)?;
        }
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
        let Some(frame) = read_stdio_frame(reader)? else {
            return Ok(());
        };
        let framing = frame.framing;
        if let Some(response) = runtime.block_on(handle_stdio_frame_with_state(
            state,
            &Frame { body: frame.body },
        ))? {
            write_stdio_response(writer, &response, framing)?;
        }
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
        Err(err) => tool_error_envelope("storage-error", &err.to_string(), storage_retryable(&err)),
    }
}

fn flatten_storage_envelope_result(result: Result<Value, StorageError>) -> Value {
    match result {
        Ok(envelope) => envelope,
        Err(err) => tool_error_envelope("storage-error", &err.to_string(), storage_retryable(&err)),
    }
}

/// `storage-error` retryable hint. FK violations are deterministic against
/// the same row set; everything else (`SQLITE_BUSY`, disk-full, pool errors)
/// stays retryable (clarion-df58379de4).
fn storage_retryable(err: &StorageError) -> bool {
    !err.is_foreign_key_violation()
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
    tool_error_envelope_with_diagnostics(code, message, retryable, json!({}), Vec::new())
}

fn tool_error_envelope_with_diagnostics(
    code: &str,
    message: &str,
    retryable: bool,
    stats_delta: Value,
    diagnostics: Vec<Value>,
) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert("ok".to_owned(), Value::Bool(false));
    envelope.insert("result".to_owned(), Value::Null);
    envelope.insert(
        "error".to_owned(),
        json!({
            "code": code,
            "message": message,
            "retryable": retryable,
        }),
    );
    envelope.insert("diagnostics".to_owned(), Value::Array(diagnostics));
    envelope.insert("truncated".to_owned(), Value::Bool(false));
    envelope.insert("truncation_reason".to_owned(), Value::Null);
    envelope.insert("stats_delta".to_owned(), stats_delta);
    Value::Object(envelope)
}

fn llm_usage_json(response: &LlmResponse) -> Value {
    json!({
        "tokens_input": response.input_tokens,
        "tokens_cached_input": response.cached_input_tokens,
        "tokens_output": response.output_tokens,
        "tokens_total": response.total_tokens,
        "cost_usd": response.cost_usd
    })
}

fn summary_usage_stats(response: &LlmResponse, invalid_json: bool) -> Value {
    let mut stats = serde_json::Map::new();
    stats.insert("summary_cache_misses_total".to_owned(), json!(1));
    stats.insert(
        "summary_tokens_input".to_owned(),
        json!(response.input_tokens),
    );
    stats.insert(
        "summary_tokens_cached_input".to_owned(),
        json!(response.cached_input_tokens),
    );
    stats.insert(
        "summary_tokens_output".to_owned(),
        json!(response.output_tokens),
    );
    stats.insert(
        "summary_tokens_total".to_owned(),
        json!(response.total_tokens),
    );
    stats.insert("summary_cost_usd".to_owned(), json!(response.cost_usd));
    if invalid_json {
        stats.insert("llm_invalid_json_total".to_owned(), json!(1));
    }
    Value::Object(stats)
}

fn inferred_usage_stats(response: &LlmResponse, invalid_json: bool) -> Value {
    let mut stats = serde_json::Map::new();
    stats.insert("inferred_dispatch_misses_total".to_owned(), json!(1));
    stats.insert(
        "inferred_tokens_input".to_owned(),
        json!(response.input_tokens),
    );
    stats.insert(
        "inferred_tokens_cached_input".to_owned(),
        json!(response.cached_input_tokens),
    );
    stats.insert(
        "inferred_tokens_output".to_owned(),
        json!(response.output_tokens),
    );
    stats.insert(
        "inferred_tokens_total".to_owned(),
        json!(response.total_tokens),
    );
    stats.insert("inferred_cost_usd".to_owned(), json!(response.cost_usd));
    if invalid_json {
        stats.insert("llm_invalid_json_total".to_owned(), json!(1));
    }
    Value::Object(stats)
}

fn token_ceiling_envelope(message: &str) -> Value {
    json!({
        "ok": false,
        "result": null,
        "error": {
            "code": "token-ceiling-exceeded",
            "message": message,
            "retryable": false
        },
        "diagnostics": [
            {
                "code": "CLA-LLM-TOKEN-CEILING-EXCEEDED",
                "message": message
            }
        ],
        "truncated": false,
        "truncation_reason": null,
        "stats_delta": {
            "token_ceiling_exceeded_total": 1
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
        SummaryRead::ScopeDeferred(entity) => summary_scope_deferred(&entity),
        SummaryRead::BriefingBlocked(entity, reason) => summary_briefing_blocked(&entity, &reason),
        SummaryRead::Ready(_) => unreachable!("ready summary read is not an error"),
    }
}

#[derive(Debug)]
struct SourceExcerptError {
    entity_id: String,
    stored_content_hash: String,
    current_content_hash: String,
}

impl SourceExcerptError {
    fn message(&self) -> String {
        format!(
            "entity {} source content drifted: stored content_hash {} but current file hashes to {}; rerun `clarion analyze` before requesting LLM output",
            self.entity_id, self.stored_content_hash, self.current_content_hash
        )
    }

    fn to_envelope(&self) -> Value {
        tool_error_envelope("content-drift", &self.message(), false)
    }

    fn to_inferred_failure(&self) -> InferredDispatchFailure {
        InferredDispatchFailure::new("content-drift", &self.message(), false)
    }
}

fn summary_success_envelope(
    entity: &EntityRow,
    entry: &SummaryCacheEntry,
    cache_hit: bool,
    stale_semantic: bool,
    cached_input_tokens: Option<i64>,
    stats_delta: Value,
) -> Value {
    let summary = serde_json::from_str::<Value>(&entry.summary_json).unwrap_or_else(|_| {
        json!({
            "raw": entry.summary_json
        })
    });
    let mut usage = serde_json::Map::new();
    usage.insert("tokens_input".to_owned(), json!(entry.tokens_input));
    if let Some(tokens) = cached_input_tokens {
        usage.insert("tokens_cached_input".to_owned(), json!(tokens));
    }
    usage.insert("tokens_output".to_owned(), json!(entry.tokens_output));
    usage.insert(
        "tokens_total".to_owned(),
        json!(entry.tokens_input + entry.tokens_output),
    );
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
            "usage": Value::Object(usage)
        }),
        stats_delta,
    )
}

fn summary_scope_deferred(entity: &EntityRow) -> Value {
    success_envelope(json!({
        "available": false,
        "reason": "summary-scope-deferred",
        "message": "subsystem summaries are deferred to v0.2",
        "entity": entity_json(entity)
    }))
}

fn summary_briefing_blocked(entity: &EntityRow, reason: &str) -> Value {
    let remediation = if reason == "unscanned_source" {
        "Entity source file was not covered by the pre-ingest secret scan. Re-run with scanner coverage for that path or fix the plugin source path before requesting a summary."
    } else {
        "File flagged by pre-ingest secret scan. Fix the secret or whitelist via .clarion/secrets-baseline.yaml. See ADR-013."
    };
    success_envelope(json!({
        "available": false,
        "entity_id": entity.id,
        "entity": entity_json(entity),
        "summary": null,
        "briefing_blocked": reason,
        "remediation": remediation
    }))
}

fn briefing_block_reason(entity: &EntityRow) -> Option<String> {
    entity_properties_json(entity)
        .get("briefing_blocked")
        .and_then(Value::as_str)
        .map(str::to_owned)
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

fn entity_properties_json(entity: &EntityRow) -> Value {
    serde_json::from_str::<Value>(&entity.properties_json)
        .expect("entity properties_json should be valid JSON")
}

fn verified_source_excerpt(entity: &EntityRow) -> Result<String, SourceExcerptError> {
    let Some(path) = entity.source_file_path.as_deref() else {
        return Ok(String::new());
    };
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(String::new());
    };
    let source = String::from_utf8(bytes.clone()).ok();
    if let (Some(stored_content_hash), Some(current_content_hash)) = (
        entity.content_hash.as_deref(),
        current_source_content_hash(entity, &bytes, source.as_deref()),
    ) && stored_content_hash != current_content_hash
    {
        return Err(SourceExcerptError {
            entity_id: entity.id.clone(),
            stored_content_hash: stored_content_hash.to_owned(),
            current_content_hash,
        });
    }
    let Some(source) = source else {
        return Ok(String::new());
    };
    let excerpt = line_range_excerpt(&source, entity.source_line_start, entity.source_line_end)
        .unwrap_or(source);
    Ok(truncate_excerpt(excerpt))
}

fn current_source_content_hash(
    entity: &EntityRow,
    file_bytes: &[u8],
    source: Option<&str>,
) -> Option<String> {
    if entity.kind == "module" {
        return Some(blake3::hash(file_bytes).to_hex().to_string());
    }
    let source = source?;
    let start_line = entity.source_line_start?;
    let end_line = entity.source_line_end?;
    if start_line <= 0 || end_line < start_line {
        return None;
    }
    let start = usize::try_from(start_line - 1).ok()?;
    let mut end = usize::try_from(end_line).ok()?;
    let lines = source.lines().collect::<Vec<_>>();
    end = end.min(lines.len());
    if start >= end {
        return None;
    }
    let normalized = lines[start..end].join("\n");
    Some(blake3::hash(normalized.as_bytes()).to_hex().to_string())
}

fn line_range_excerpt(
    source: &str,
    start_line: Option<i64>,
    end_line: Option<i64>,
) -> Option<String> {
    let start_line = start_line?;
    let end_line = end_line?;
    if start_line <= 0 || end_line < start_line {
        return None;
    }
    let start = usize::try_from(start_line - 1).ok()?;
    let end = usize::try_from(end_line).ok()?;
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    let end = end.min(lines.len());
    if start >= end {
        return None;
    }
    Some(lines[start..end].concat())
}

fn truncate_excerpt(source: String) -> String {
    if source.len() > 8_000 {
        source.chars().take(8_000).collect()
    } else {
        source
    }
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
    let date = Date::parse(date, format_description!("[year]-[month]-[day]")).ok()?;
    let unix_epoch = Date::from_calendar_date(1970, Month::January, 1)
        .expect("Unix epoch is a valid calendar date");
    Some(i64::from(date.to_julian_day() - unix_epoch.to_julian_day()))
}

fn default_now_string() -> String {
    let seconds = OffsetDateTime::now_utc().unix_timestamp();
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
    let mut neighbors = Vec::new();
    for edge in reference_edges_for_entity(conn, entity_id, direction)? {
        if let Some(entity) = entity_by_id(conn, &edge.neighbor_id)? {
            neighbors.push(json!({
                "entity": entity_json(&entity),
                "edge_confidence": edge.confidence.as_str(),
                "source_byte_start": edge.source_byte_start,
                "source_byte_end": edge.source_byte_end
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
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use clarion_core::{CachingModel, LlmProvider, LlmProviderError, LlmRequest, LlmResponse};
    use clarion_storage::{
        EntityRow, InferredEdgeCacheKey, ReaderPool, UnresolvedCallSiteRow, pragma, schema,
    };
    use rusqlite::Connection;
    use tokio::sync::mpsc;

    use super::{InferenceLlmState, InferredRead, ServerState, config::LlmConfig, list_tools};

    #[test]
    fn tools_list_exposes_exact_docstrings() {
        let tools = list_tools();

        assert_eq!(tools.len(), 8);
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
        assert_eq!(tools[7].name, "subsystem_members");
        assert_eq!(
            tools[7].description,
            "List module entities assigned to a subsystem entity."
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
        }))
        .expect("initialize request returns a response");

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
        }))
        .expect("tools/list request returns a response");

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], "tools-1");
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 8);
        assert_eq!(response["result"]["tools"][0]["name"], "entity_at");
        assert_eq!(response["result"]["tools"][7]["name"], "subsystem_members");
    }

    #[test]
    fn unknown_method_is_json_rpc_method_not_found() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "not/real",
            "params": {}
        }))
        .expect("unknown request returns a JSON-RPC error response");

        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn json_rpc_notification_does_not_return_response() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }));

        assert!(response.is_none());
    }

    #[test]
    fn stateless_call_tool_requires_server_state_before_tool_validation() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {"name": "not_a_tool", "arguments": {}}
        }))
        .expect("tools/call request returns a response");

        assert_eq!(response["error"]["code"], -32601);
        assert_eq!(
            response["error"]["message"],
            "tools/call requires ServerState::handle_json_rpc"
        );
    }

    #[test]
    fn stateless_json_rpc_does_not_fake_tool_calls() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 88,
            "method": "tools/call",
            "params": {"name": "summary", "arguments": {"id": "python:function:demo.entry"}}
        }))
        .expect("tools/call request returns a response");

        assert_eq!(response["error"]["code"], -32601);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("ServerState")
        );
    }

    #[test]
    fn stateless_call_tool_with_invalid_params_requires_server_state() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {"arguments": {}}
        }))
        .expect("tools/call request returns a response");

        assert_eq!(response["error"]["code"], -32601);
        assert_eq!(
            response["error"]["message"],
            "tools/call requires ServerState::handle_json_rpc"
        );
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

        let response = super::handle_frame(&frame)
            .unwrap()
            .expect("request frame returns a response");
        let decoded: serde_json::Value = serde_json::from_slice(&response.body).unwrap();

        assert_eq!(decoded["jsonrpc"], "2.0");
        assert_eq!(decoded["id"], 10);
        assert_eq!(decoded["result"]["tools"].as_array().unwrap().len(), 8);
    }

    #[test]
    fn frame_dispatch_returns_none_for_json_rpc_notifications() {
        let frame = clarion_core::plugin::Frame {
            body: serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }))
            .unwrap(),
        };

        let response = super::handle_frame(&frame).unwrap();

        assert!(response.is_none());
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
        assert_eq!(second_json["result"]["tools"].as_array().unwrap().len(), 8);
    }

    #[test]
    fn serve_stdio_ignores_json_rpc_notifications() {
        let input = notification_sequence_input(13, 14);
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
        let mut output = Vec::new();

        super::serve_stdio(&mut reader, &mut output).unwrap();
        assert_notification_sequence_responses(output, 13, 14);
    }

    #[test]
    fn serve_stdio_with_state_ignores_json_rpc_notifications() {
        let project = tempfile::tempdir().expect("temp project");
        let db_path = project.path().join("clarion.db");
        let mut conn = Connection::open(&db_path).expect("open sqlite");
        pragma::apply_write_pragmas(&conn).expect("write pragmas");
        schema::apply_migrations(&mut conn).expect("apply migrations");
        drop(conn);

        let readers = ReaderPool::open(&db_path, 1).expect("reader pool");
        let state = ServerState::new(project.path().to_path_buf(), readers);
        let input = notification_sequence_input(15, 16);
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
        let mut output = Vec::new();

        super::serve_stdio_with_state(&state, &mut reader, &mut output).unwrap();
        assert_notification_sequence_responses(output, 15, 16);
    }

    #[test]
    fn serve_stdio_with_state_uses_json_line_transport_for_json_line_requests() {
        let project = tempfile::tempdir().expect("temp project");
        let db_path = project.path().join("clarion.db");
        let mut conn = Connection::open(&db_path).expect("open sqlite");
        pragma::apply_write_pragmas(&conn).expect("write pragmas");
        schema::apply_migrations(&mut conn).expect("apply migrations");
        drop(conn);

        let readers = ReaderPool::open(&db_path, 1).expect("reader pool");
        let state = ServerState::new(project.path().to_path_buf(), readers);
        let input = notification_sequence_json_lines(17, 18);
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
        let mut output = Vec::new();

        super::serve_stdio_with_state(&state, &mut reader, &mut output).unwrap();
        assert_notification_sequence_json_lines(output, 17, 18);
    }

    fn notification_sequence_input(initialize_id: u64, tools_list_id: u64) -> Vec<u8> {
        let mut input = Vec::new();
        clarion_core::plugin::write_frame(
            &mut input,
            &clarion_core::plugin::Frame {
                body: serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": initialize_id,
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
                    "method": "notifications/initialized",
                    "params": {}
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
                    "id": tools_list_id,
                    "method": "tools/list",
                    "params": {}
                }))
                .unwrap(),
            },
        )
        .unwrap();
        input
    }

    fn assert_notification_sequence_responses(
        output: Vec<u8>,
        initialize_id: u64,
        tools_list_id: u64,
    ) {
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

        assert_eq!(first_json["id"], initialize_id);
        assert_eq!(second_json["id"], tools_list_id);
        assert!(
            clarion_core::plugin::read_frame(
                &mut response_reader,
                clarion_core::plugin::ContentLengthCeiling::new(usize::MAX),
            )
            .is_err(),
            "notifications must not produce JSON-RPC response frames"
        );
    }

    fn notification_sequence_json_lines(initialize_id: u64, tools_list_id: u64) -> Vec<u8> {
        let messages = [
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": initialize_id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "test-client", "version": "0.0.0"}
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": tools_list_id,
                "method": "tools/list",
                "params": {}
            }),
        ];
        let mut input = Vec::new();
        for message in messages {
            serde_json::to_writer(&mut input, &message).expect("serialize json line");
            input.push(b'\n');
        }
        input
    }

    fn assert_notification_sequence_json_lines(
        output: Vec<u8>,
        initialize_id: u64,
        tools_list_id: u64,
    ) {
        let output = String::from_utf8(output).expect("json lines are utf8");
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "notifications must not produce JSON-RPC response lines"
        );
        let first_json: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let second_json: serde_json::Value = serde_json::from_str(lines[1]).unwrap();

        assert_eq!(first_json["id"], initialize_id);
        assert_eq!(second_json["id"], tools_list_id);
    }

    #[tokio::test]
    async fn inferred_inflight_entry_is_removed_when_leader_future_is_aborted() {
        let project = tempfile::tempdir().expect("temp project");
        let db_path = project.path().join("clarion.db");
        let mut conn = Connection::open(&db_path).expect("open sqlite");
        pragma::apply_write_pragmas(&conn).expect("write pragmas");
        schema::apply_migrations(&mut conn).expect("apply migrations");
        drop(conn);

        let readers = ReaderPool::open(&db_path, 1).expect("reader pool");
        let state = Arc::new(ServerState::new(project.path().to_path_buf(), readers));
        let key = inferred_test_key();
        let read = inferred_test_read(key.clone());
        let (writer, _rx) = mpsc::channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let llm = InferenceLlmState {
            writer,
            config: LlmConfig::default(),
            provider: Arc::new(BlockingProvider {
                release: Mutex::new(release_rx),
            }),
        };

        let leader_state = Arc::clone(&state);
        let leader_key = key.clone();
        let handle = tokio::spawn(async move {
            leader_state
                .coalesced_inferred_dispatch(leader_key, read, llm)
                .await
        });
        assert_inferred_inflight_contains(&state, &key).await;

        handle.abort();
        let _ = handle.await;
        let removed = wait_until_inferred_inflight_removed(&state, &key).await;
        let _ = release_tx.send(());

        assert!(
            removed,
            "aborted inferred-dispatch leader left stale in-flight key"
        );
    }

    async fn assert_inferred_inflight_contains(state: &ServerState, key: &InferredEdgeCacheKey) {
        for _ in 0..50 {
            if state.inferred_inflight.lock().await.contains_key(key) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("inferred-dispatch leader never registered in-flight key");
    }

    async fn wait_until_inferred_inflight_removed(
        state: &ServerState,
        key: &InferredEdgeCacheKey,
    ) -> bool {
        for _ in 0..50 {
            if !state.inferred_inflight.lock().await.contains_key(key) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    fn inferred_test_key() -> InferredEdgeCacheKey {
        InferredEdgeCacheKey {
            caller_entity_id: "python:function:demo.dynamic".to_owned(),
            caller_content_hash: "hash-caller".to_owned(),
            model_id: "test-model".to_owned(),
            prompt_version: "test-prompt".to_owned(),
        }
    }

    fn inferred_test_read(key: InferredEdgeCacheKey) -> InferredRead {
        InferredRead {
            caller: entity_row(&key.caller_entity_id, "dynamic", Some("hash-caller")),
            sites: vec![UnresolvedCallSiteRow {
                caller_entity_id: key.caller_entity_id.clone(),
                caller_content_hash: key.caller_content_hash.clone(),
                site_key: "site-1".to_owned(),
                site_ordinal: 0,
                source_file_id: Some("python:module:demo".to_owned()),
                source_byte_start: 0,
                source_byte_end: 8,
                callee_expr: "target()".to_owned(),
            }],
            candidates: vec![entity_row(
                "python:function:demo.target",
                "target",
                Some("hash-target"),
            )],
            key,
            cached: None,
        }
    }

    fn entity_row(id: &str, name: &str, content_hash: Option<&str>) -> EntityRow {
        EntityRow {
            id: id.to_owned(),
            plugin_id: "python".to_owned(),
            kind: "function".to_owned(),
            name: name.to_owned(),
            short_name: name.to_owned(),
            parent_id: Some("python:module:demo".to_owned()),
            source_file_id: Some("python:module:demo".to_owned()),
            source_file_path: None,
            source_byte_start: Some(0),
            source_byte_end: Some(8),
            source_line_start: Some(1),
            source_line_end: Some(1),
            properties_json: "{}".to_owned(),
            content_hash: content_hash.map(str::to_owned),
            summary_json: None,
        }
    }

    struct BlockingProvider {
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl LlmProvider for BlockingProvider {
        fn name(&self) -> &'static str {
            "blocking"
        }

        fn invoke(&self, _request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
            let _ = self
                .release
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv();
            Ok(LlmResponse {
                model_id: "test-model".to_owned(),
                output_json: r#"{"edges":[]}"#.to_owned(),
                input_tokens: 1,
                cached_input_tokens: 0,
                output_tokens: 1,
                total_tokens: 2,
                cost_usd: 0.0,
            })
        }

        fn estimate_tokens(&self, _request: &LlmRequest) -> u64 {
            1
        }

        fn tier_to_model(&self, _tier: &str) -> Option<&str> {
            Some("test-model")
        }

        fn caching_model(&self) -> CachingModel {
            CachingModel::OpenAiChatCompletions
        }
    }
}
