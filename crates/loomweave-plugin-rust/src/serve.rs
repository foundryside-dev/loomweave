//! The plugin's JSON-RPC serve loop, extracted from `main.rs` so it can be the
//! single entry point for BOTH cargo bins: the off-glob `loomweave-rust-plugin`
//! (dev/test artifact) and the discovery-glob `loomweave-plugin-rust` bin built
//! by the out-of-workspace distribution crate (`packaging/rust-plugin-dist`)
//! that maturin packages into the `loomweave-plugin-rust` wheel. Keeping the
//! loop in the library means the two bins are one-line shims and cannot drift.
//!
//! Speaks the Loomweave JSON-RPC 2.0 protocol over `stdin`/`stdout`, the same
//! Content-Length-framed wire contract as `loomweave-plugin-fixture` and the
//! Python plugin. Phase 1a wires the four handlers (`initialize`/`initialized`/
//! `analyze_file`/`shutdown`+`exit`). `initialize` stashes `project_root`,
//! builds the symbol table (Task 7), and discovers crate roots (Task 2).
//! `analyze_file` derives the file's crate name + dotted module path from those
//! roots and extracts entities through the degraded-parse fallback (Task 9), so
//! a malformed file yields one degraded `module` entity plus a Warning finding
//! rather than an empty result.

use std::io::{BufReader, Write};

use loomweave_core::plugin::limits::ContentLengthCeiling;
use loomweave_core::plugin::transport::{Frame, read_frame, write_frame};
use loomweave_core::plugin::{
    AnalyzeFileFinding, AnalyzeFileParams, AnalyzeFileResult, AnalyzeFileStats, InitializeParams,
    InitializeResult, JsonRpcVersion, ResponseEnvelope, ResponsePayload, ShutdownResult,
    UnresolvedCallSite,
};
use serde_json::Value;

use crate::crate_roots::CrateRoots;
use crate::mounts::ModMounts;
use crate::references::ReferenceStats;
use crate::symbol_table::SymbolTable;

/// Run the plugin's blocking JSON-RPC serve loop on stdin/stdout until `exit`
/// (or an unrecoverable framing/protocol error). Never returns normally; calls
/// `std::process::exit` on `exit`/error, matching the host's lifecycle contract.
///
/// # Panics
///
/// Panics only if writing a well-formed response to stdout fails (serialising a
/// constructed `ResponseEnvelope`, framing it, or flushing) — an unrecoverable
/// transport fault. Malformed *input* never panics: it exits non-zero per the
/// host lifecycle contract.
pub fn run() -> ! {
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
    // `#[path]` mount overlay (ADR-049 Amendment 8), discovered at `initialize`
    // BEFORE the symbol table is built and threaded into both the table build
    // and every `analyze_file` scope derivation — see the ordering note below.
    let mut mod_mounts: Option<ModMounts> = None;

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
                    //
                    // ORDERING IS LOAD-BEARING (ADR-049 Amendment 8): the
                    // `#[path]` mount overlay is discovered FIRST and the SAME
                    // instance feeds both the symbol-table build and every
                    // later `analyze_file` scope derivation. If the table were
                    // built mount-blind, its qualnames would desync from the
                    // mount-correct paths `analyze_file` emits and `use`
                    // resolution would anchor edges at ids that never exist.
                    project_root = params.project_root;
                    let root = std::path::Path::new(&project_root);
                    let roots = crate::crate_roots::discover_crate_roots(root);
                    let mounts = crate::mounts::discover_mounts(root, &roots);
                    symbol_table = Some(crate::symbol_table::build_symbol_table_with(
                        root, &roots, &mounts,
                    ));
                    crate_roots = Some(roots);
                    mod_mounts = Some(mounts);
                }
                let _ = &project_root;
                let result = InitializeResult {
                    name: "loomweave-plugin-rust".to_owned(),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    // Lockstep with plugin.toml `[ontology].ontology_version`
                    // (ADR-027). Bump both together.
                    ontology_version: "0.5.0".to_owned(),
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
                let (entities, edges, unresolved_call_sites, reference_stats, findings) =
                    analyze_one_file(
                        &params.file_path,
                        crate_roots.as_ref(),
                        mod_mounts.as_ref(),
                        symbol_table.as_ref(),
                    );
                let result = AnalyzeFileResult {
                    entities,
                    edges,
                    stats: AnalyzeFileStats {
                        // Phase 2 calls stats. `*_total` is the count of sites
                        // that produced no in-project `calls` edge; the list is
                        // those sites verbatim for lazy query-time inference.
                        unresolved_call_sites_total: unresolved_call_sites.len() as u64,
                        unresolved_call_sites,
                        // Phase 2 references stats (D4).
                        // `references_skipped_external_total` absorbs BOTH
                        // external-crate and no-match outcomes — syn cannot
                        // distinguish them. `unresolved_reference_sites_total`
                        // and `references_skipped_cap_total` stay 0 for Rust
                        // (pyright reports externality and needs a cost cap;
                        // syn does neither — documented divergence from the
                        // Python plugin, see the `references` module docs).
                        // The latency fields stay default (no LSP round-trips).
                        reference_sites_total: reference_stats.sites_total,
                        references_resolved_total: reference_stats.resolved_total,
                        references_skipped_external_total: reference_stats.skipped_external_total,
                        ..Default::default()
                    },
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
/// roots via the shared [`emittable_scope`](crate::scope::emittable_scope)
/// guard, then extract entities with the degraded-parse fallback (Task 9). A
/// file that resolves to no emittable crate scope — out of any crate's `src/`
/// tree (`tests/`, `benches/`, `examples/`, `build.rs`), a redundant `main.rs`
/// shadowing a sibling `lib.rs`, or outside any known crate root — emits
/// NOTHING: minting a bare-crate `rust:module:<crate>` for it would collide with
/// the real crate root and `FailRun` the storage writer
/// (`LMWV-INFRA-PARENT-CONTAINS-MISMATCH`). The degraded-parse fallback still
/// runs below for files that ARE in scope but fail to parse.
///
/// Returns `(entities, edges, unresolved_call_sites, reference_stats,
/// findings)` already shaped for the wire: the structural `contains` edges
/// (ADR-026 dual-encoding) accompany the entities, and degraded-parse
/// `findings` from
/// [`extract_file_degraded_aware`](crate::extract::extract_file_degraded_aware) are
/// deserialised into [`AnalyzeFileFinding`]; any element that fails to
/// deserialise is dropped rather than aborting the response.
///
/// In-scope files are vetted by the pre-parse guards first (ADR-050): a file
/// over [`parse_guard::MAX_FILE_BYTES`](crate::parse_guard::MAX_FILE_BYTES)
/// (checked via `fs::metadata`, never read) or one tripping the depth/prefix
/// scan degrades to a single module entity (`parse_status` `"file_too_large"` /
/// `"depth_limit"`) plus a warning finding (`LMWV-RUST-FILE-TOO-LARGE` /
/// `LMWV-RUST-DEPTH-LIMIT`) instead of crashing the plugin.
fn analyze_one_file(
    file_path: &str,
    crate_roots: Option<&CrateRoots>,
    mod_mounts: Option<&ModMounts>,
    symbol_table: Option<&SymbolTable>,
) -> (
    Vec<Value>,
    Vec<Value>,
    Vec<UnresolvedCallSite>,
    ReferenceStats,
    Vec<AnalyzeFileFinding>,
) {
    use crate::extract::{
        extract_file_degraded_aware, extract_file_degraded_aware_with_edges,
        extract_file_guard_degraded,
    };
    use crate::parse_guard;
    use crate::resolve::Resolver;
    use crate::scope::emittable_scope;

    let file = std::path::Path::new(file_path);

    // Out-of-scope files (out of any crate's `src/` tree, a redundant `main.rs`,
    // or outside any known crate root) emit NOTHING — see the doc comment above.
    // Module-path derivation is `#[path]`-mount-aware (Amendment 8); a missing
    // overlay (defensive — every real `initialize` builds one) degrades to the
    // pure filesystem route via an empty overlay.
    let empty_mounts = ModMounts::empty();
    let Some((crate_name, module_path)) = crate_roots
        .and_then(|roots| emittable_scope(roots, file, mod_mounts.unwrap_or(&empty_mounts)))
    else {
        return (
            Vec::new(),
            Vec::new(),
            Vec::new(),
            ReferenceStats::default(),
            Vec::new(),
        );
    };

    // Pre-parse guards (ADR-050): an oversize file (size checked via metadata,
    // BEFORE reading it into memory) or a depth/prefix bomb degrades to a single
    // module entity plus a warning finding instead of risking an RLIMIT_AS kill
    // or a parser stack overflow.
    let (entities, edges, unresolved_call_sites, reference_stats, finding_values) =
        if let Err(violation) = parse_guard::check_file_size(file) {
            extract_file_guard_degraded(&module_path, file_path, &violation)
        } else {
            let src = std::fs::read_to_string(file).unwrap_or_default();
            if let Err(violation) = parse_guard::scan_source(&src) {
                extract_file_guard_degraded(&module_path, file_path, &violation)
            } else {
                // With the project symbol table stashed at `initialize`, resolve
                // this file's in-project `use` paths into `imports` edges;
                // without it (defensive — every real `initialize` builds one)
                // fall back to the entities/`contains`-only path so analysis
                // never silently aborts.
                match symbol_table {
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
                }
            }
        };
    let findings = finding_values
        .into_iter()
        .filter_map(|v| serde_json::from_value::<AnalyzeFileFinding>(v).ok())
        .collect();
    (
        entities,
        edges,
        unresolved_call_sites,
        reference_stats,
        findings,
    )
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
