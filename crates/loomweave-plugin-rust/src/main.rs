//! Rust language plugin binary — Phase 1a identity foundation.
//!
//! Speaks the Loomweave JSON-RPC 2.0 protocol over `stdin`/`stdout`, the same
//! Content-Length-framed wire contract as `loomweave-plugin-fixture` and the
//! Python plugin. The cargo artifact is named `loomweave-rust-plugin` (OFF the
//! `loomweave-plugin-*` discovery glob); `loomweave install` stages it under
//! the discovery name `loomweave-plugin-rust` with a neighbouring `plugin.toml`.
//!
//! Phase 1a wires the four handlers (`initialize`/`initialized`/`analyze_file`/
//! `shutdown`+`exit`). `initialize` stashes `project_root`, builds the symbol
//! table (Task 7), and discovers crate roots (Task 2). `analyze_file` derives
//! the file's crate name + dotted module path from those roots and extracts
//! entities through the degraded-parse fallback (Task 9), so a malformed file
//! yields one degraded `module` entity plus a Warning finding rather than an
//! empty result.

use std::io::{BufReader, Write};

use loomweave_core::plugin::limits::ContentLengthCeiling;
use loomweave_core::plugin::transport::{Frame, read_frame, write_frame};
use loomweave_core::plugin::{
    AnalyzeFileFinding, AnalyzeFileParams, AnalyzeFileResult, AnalyzeFileStats, InitializeParams,
    InitializeResult, JsonRpcVersion, ResponseEnvelope, ResponsePayload, ShutdownResult,
};
use loomweave_plugin_rust::crate_roots::CrateRoots;
use serde_json::Value;

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    // Stashed at `initialize` for the Task 7 symbol-table build.
    let mut project_root = String::new();
    // Discovered at `initialize`; consulted at `analyze_file` to derive each
    // file's crate name + dotted module path (ADR-049 §1, Task 2).
    let mut crate_roots: Option<CrateRoots> = None;

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
                    let root = std::path::Path::new(&project_root);
                    let symbol_table =
                        loomweave_plugin_rust::symbol_table::build_symbol_table(root);
                    let _ = symbol_table.len();
                    crate_roots = Some(loomweave_plugin_rust::crate_roots::discover_crate_roots(
                        root,
                    ));
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
                let params: AnalyzeFileParams = serde_json::from_value(
                    raw.get("params").cloned().unwrap_or(serde_json::json!({})),
                )
                .unwrap_or(AnalyzeFileParams {
                    file_path: String::new(),
                });
                let (entities, findings) =
                    analyze_one_file(&params.file_path, crate_roots.as_ref());
                let result = AnalyzeFileResult {
                    entities,
                    edges: vec![],
                    stats: AnalyzeFileStats::default(),
                    findings,
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

/// Read one source file, derive its crate name + dotted module path from the
/// discovered crate roots (ADR-049 §1), and extract entities with the
/// degraded-parse fallback (Task 9). On a read failure or a file outside any
/// known crate root, falls back to the file stem as the module path so a
/// malformed/unreadable file still yields a degraded module rather than nothing.
///
/// Returns `(entities, findings)` already shaped for the wire:
/// degraded-parse `findings` from [`extract_file_degraded_aware`] are
/// deserialised into [`AnalyzeFileFinding`]; any element that fails to
/// deserialise is dropped rather than aborting the response.
fn analyze_one_file(
    file_path: &str,
    crate_roots: Option<&CrateRoots>,
) -> (Vec<Value>, Vec<AnalyzeFileFinding>) {
    use loomweave_plugin_rust::extract::extract_file_degraded_aware;
    use loomweave_plugin_rust::module_path::module_path_for;

    let file = std::path::Path::new(file_path);
    let stem = file
        .file_stem()
        .map_or_else(|| "module".to_owned(), |s| s.to_string_lossy().into_owned());

    let (crate_name, module_path) = match crate_roots {
        Some(roots) => match (roots.crate_name_for(file), roots.crate_dir_for(file)) {
            (Some(name), Some(dir)) => {
                let src_root = dir.join("src");
                let module = module_path_for(&name, &src_root, file);
                (name, module)
            }
            _ => (stem.clone(), stem),
        },
        None => (stem.clone(), stem),
    };

    let src = std::fs::read_to_string(file).unwrap_or_default();
    let (entities, finding_values) =
        extract_file_degraded_aware(&crate_name, &module_path, file_path, &src);
    let findings = finding_values
        .into_iter()
        .filter_map(|v| serde_json::from_value::<AnalyzeFileFinding>(v).ok())
        .collect();
    (entities, findings)
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
