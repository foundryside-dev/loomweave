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
use loomweave_plugin_rust::symbol_table::SymbolTable;
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
    // Built at `initialize`; consulted at `analyze_file` to resolve each file's
    // in-project `use` paths into `imports` edges (Task 7). Mirrors
    // `crate_roots`: stashed here, threaded into `analyze_one_file`.
    let mut symbol_table: Option<SymbolTable> = None;

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
                    // spec §2.3) and STASH it: `analyze_file` builds a
                    // `Resolver` over it to resolve each file's in-project `use`
                    // paths into `imports` edges (Phase 1b). It also powers the
                    // dogfood gate (Task 14).
                    project_root = params.project_root;
                    let root = std::path::Path::new(&project_root);
                    symbol_table =
                        Some(loomweave_plugin_rust::symbol_table::build_symbol_table(root));
                    crate_roots = Some(loomweave_plugin_rust::crate_roots::discover_crate_roots(
                        root,
                    ));
                }
                let _ = &project_root;
                let result = InitializeResult {
                    name: "loomweave-plugin-rust".to_owned(),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    // Lockstep with plugin.toml `[ontology].ontology_version`
                    // (ADR-027). Bump both together.
                    ontology_version: "0.4.0".to_owned(),
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
                let (entities, edges, findings) = analyze_one_file(
                    &params.file_path,
                    crate_roots.as_ref(),
                    symbol_table.as_ref(),
                );
                let result = AnalyzeFileResult {
                    entities,
                    edges,
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

/// Derive a file's crate name + dotted module path from the discovered crate
/// roots via the shared [`emittable_scope`](loomweave_plugin_rust::scope::emittable_scope)
/// guard, then extract entities with the degraded-parse fallback (Task 9). A
/// file that resolves to no emittable crate scope — out of any crate's `src/`
/// tree (`tests/`, `benches/`, `examples/`, `build.rs`), a redundant `main.rs`
/// shadowing a sibling `lib.rs`, or outside any known crate root — emits
/// NOTHING: minting a bare-crate `rust:module:<crate>` for it would collide with
/// the real crate root and `FailRun` the storage writer
/// (`LMWV-INFRA-PARENT-CONTAINS-MISMATCH`). The degraded-parse fallback still
/// runs below for files that ARE in scope but fail to parse.
///
/// Returns `(entities, edges, findings)` already shaped for the wire: the
/// structural `contains` edges (ADR-026 dual-encoding) accompany the entities,
/// and degraded-parse `findings` from
/// [`extract_file_degraded_aware`](loomweave_plugin_rust::extract::extract_file_degraded_aware) are
/// deserialised into [`AnalyzeFileFinding`]; any element that fails to
/// deserialise is dropped rather than aborting the response.
fn analyze_one_file(
    file_path: &str,
    crate_roots: Option<&CrateRoots>,
    symbol_table: Option<&SymbolTable>,
) -> (Vec<Value>, Vec<Value>, Vec<AnalyzeFileFinding>) {
    use loomweave_plugin_rust::extract::{
        extract_file_degraded_aware, extract_file_degraded_aware_with_edges,
    };
    use loomweave_plugin_rust::resolve::Resolver;
    use loomweave_plugin_rust::scope::emittable_scope;

    let file = std::path::Path::new(file_path);

    // Out-of-scope files (out of any crate's `src/` tree, a redundant `main.rs`,
    // or outside any known crate root) emit NOTHING — see the doc comment above.
    let Some((crate_name, module_path)) =
        crate_roots.and_then(|roots| emittable_scope(roots, file))
    else {
        return (Vec::new(), Vec::new(), Vec::new());
    };

    let src = std::fs::read_to_string(file).unwrap_or_default();
    // With the project symbol table stashed at `initialize`, resolve this file's
    // in-project `use` paths into `imports` edges; without it (defensive — every
    // real `initialize` builds one) fall back to the entities/`contains`-only
    // path so analysis never silently aborts.
    let (entities, edges, finding_values) = match symbol_table {
        Some(table) => {
            let resolver = Resolver::new(table);
            extract_file_degraded_aware_with_edges(
                &crate_name,
                &module_path,
                file_path,
                &src,
                &resolver,
            )
        }
        None => extract_file_degraded_aware(&crate_name, &module_path, file_path, &src),
    };
    let findings = finding_values
        .into_iter()
        .filter_map(|v| serde_json::from_value::<AnalyzeFileFinding>(v).ok())
        .collect();
    (entities, edges, findings)
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
