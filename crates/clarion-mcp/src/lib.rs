//! MCP protocol surface for Clarion.

pub mod config;

use std::path::PathBuf;

use clarion_core::EdgeConfidence;
use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;

use clarion_core::plugin::{ContentLengthCeiling, Frame, TransportError};
use clarion_storage::{
    CallEdgeMatch, EntityRow, ReaderPool, StorageError, call_edges_from, call_edges_targeting,
    child_entity_ids, entity_at_line, entity_by_id, find_entities, normalize_source_path,
};

/// MCP protocol revision supported by the B.6 stdio server.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

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
}

impl ServerState {
    #[must_use]
    pub fn new(project_root: PathBuf, readers: ReaderPool) -> Self {
        Self {
            project_root,
            readers,
            execution_edge_cap: 500,
        }
    }

    #[must_use]
    pub fn with_edge_cap(mut self, execution_edge_cap: usize) -> Self {
        self.execution_edge_cap = execution_edge_cap;
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
            "summary" | "issues_for" => tool_error_envelope(
                "tool-unimplemented",
                &format!("{name} is reserved for B.6b and is not implemented yet"),
                false,
            ),
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
        if confidence == EdgeConfidence::Inferred {
            return Ok(tool_error_envelope(
                "inferred-unavailable",
                "inferred call dispatch lands in B.6b; request confidence=ambiguous for static expansion",
                false,
            ));
        }
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
                Ok(success_envelope(json!({"callers": callers})))
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
            return Ok(tool_error_envelope(
                "inferred-unavailable",
                "inferred call dispatch lands in B.6b; request confidence=ambiguous for static expansion",
                false,
            ));
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

    async fn tool_neighborhood(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let confidence = optional_confidence(arguments)?;
        if confidence == EdgeConfidence::Inferred {
            return Ok(tool_error_envelope(
                "inferred-unavailable",
                "inferred call dispatch lands in B.6b; request confidence=ambiguous for static expansion",
                false,
            ));
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
