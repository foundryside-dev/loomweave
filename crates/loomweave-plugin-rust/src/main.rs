//! Rust language plugin binary — Phase 1a identity foundation.
//!
//! Speaks the Loomweave JSON-RPC 2.0 protocol over `stdin`/`stdout`, the same
//! Content-Length-framed wire contract as `loomweave-plugin-fixture` and the
//! Python plugin. The cargo artifact is named `loomweave-rust-plugin` (OFF the
//! `loomweave-plugin-*` discovery glob); `loomweave install` stages it under
//! the discovery name `loomweave-plugin-rust` with a neighbouring `plugin.toml`.
//!
//! Phase 1a wires the four handlers (`initialize`/`initialized`/`analyze_file`/
//! `shutdown`+`exit`). `initialize` stashes `project_root` for the symbol table
//! (built in Task 7); `analyze_file` currently returns no entities (filled in
//! Task 6).

use std::io::{BufReader, Write};

use loomweave_core::plugin::limits::ContentLengthCeiling;
use loomweave_core::plugin::transport::{Frame, read_frame, write_frame};
use loomweave_core::plugin::{
    AnalyzeFileResult, AnalyzeFileStats, InitializeParams, InitializeResult, JsonRpcVersion,
    ResponseEnvelope, ResponsePayload, ShutdownResult,
};
use serde_json::Value;

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    // Stashed at `initialize` for the Task 7 symbol-table build.
    let mut project_root = String::new();

    loop {
        let Ok(frame) = read_frame(&mut reader, ContentLengthCeiling::DEFAULT) else {
            std::process::exit(1)
        };

        let raw: Value = match serde_json::from_slice(&frame.body) {
            Ok(v) => v,
            Err(_) => std::process::exit(1),
        };

        let has_id = raw.get("id").is_some_and(|v| !v.is_null());
        let method = match raw.get("method").and_then(|v| v.as_str()) {
            Some(m) => m.to_owned(),
            None => std::process::exit(1),
        };

        if !has_id {
            // Notification — no response required.
            match method.as_str() {
                "initialized" => {
                    // Transition to ready; no response.
                }
                "exit" => {
                    std::process::exit(0);
                }
                _ => std::process::exit(1),
            }
            continue;
        }

        let Some(id) = raw.get("id").and_then(serde_json::Value::as_i64) else {
            std::process::exit(1)
        };

        match method.as_str() {
            "initialize" => {
                if let Ok(params) = serde_json::from_value::<InitializeParams>(
                    raw.get("params").cloned().unwrap_or(serde_json::json!({})),
                ) {
                    // Build the project symbol table from this root (Task 7,
                    // spec §2.3). Phase 1a does not consult it during
                    // `analyze_file` yet — Phase 1b resolves cross-file edges
                    // against it — but building it here proves the §2.3 walk
                    // and powers the dogfood gate (Task 14).
                    project_root = params.project_root;
                    let symbol_table = loomweave_plugin_rust::symbol_table::build_symbol_table(
                        std::path::Path::new(&project_root),
                    );
                    let _ = symbol_table.len();
                }
                let _ = &project_root;
                let result = InitializeResult {
                    name: "loomweave-plugin-rust".to_owned(),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    ontology_version: "0.1.0".to_owned(),
                    capabilities: serde_json::json!({}),
                };
                send_result(&mut writer, id, serde_json::to_value(result).unwrap());
            }
            "analyze_file" => {
                // Task 6 fills the entity extraction.
                let result = AnalyzeFileResult {
                    entities: vec![],
                    edges: vec![],
                    stats: AnalyzeFileStats::default(),
                    findings: vec![],
                };
                send_result(&mut writer, id, serde_json::to_value(result).unwrap());
            }
            "shutdown" => {
                let result = ShutdownResult {};
                send_result(&mut writer, id, serde_json::to_value(result).unwrap());
            }
            _ => std::process::exit(1),
        }
    }
}

fn send_result(writer: &mut impl Write, id: i64, result: Value) {
    let env = ResponseEnvelope {
        jsonrpc: JsonRpcVersion,
        id,
        payload: ResponsePayload::Result(result),
    };
    let body = serde_json::to_vec(&env).expect("serialise response");
    let frame = Frame { body };
    write_frame(writer, &frame).expect("write frame");
    writer.flush().expect("flush");
}
