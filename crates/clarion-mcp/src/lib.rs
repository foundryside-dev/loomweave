//! MCP protocol surface for Clarion.

pub mod config;

use serde::Serialize;
use serde_json::{Value, json};
use thiserror::Error;

use clarion_core::plugin::{ContentLengthCeiling, Frame, TransportError};

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

#[derive(Debug, Error)]
pub enum McpError {
    #[error("invalid JSON-RPC frame body: {0}")]
    Json(#[from] serde_json::Error),

    #[error("MCP transport error: {0}")]
    Transport(#[from] TransportError),
}

pub fn handle_frame(frame: &Frame) -> Result<Frame, McpError> {
    let request = serde_json::from_slice(&frame.body)?;
    let response = handle_json_rpc(&request);
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
