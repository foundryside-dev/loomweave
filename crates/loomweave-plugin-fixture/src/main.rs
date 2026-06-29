//! Minimal test-fixture plugin binary.
//!
//! Speaks the Loomweave JSON-RPC 2.0 protocol over `stdin`/`stdout`. Used by
//! the subprocess integration test (`host_subprocess.rs`) that exercises
//! `PluginHost::spawn`.
//!
//! Fixture identity:
//! - `plugin_id = "fixture"`, kinds `"widget"` (`file_scope`) and `"gadget"`,
//!   rule-ID prefix `"LMWV-FIXTURE-"`
//! - Responds to every `analyze_file` request with one entity:
//!   `id = "fixture:widget:demo.sample"`, `kind = "widget"`,
//!   `qualified_name = "demo.sample"`, `source.file_path` = the path sent in.
//! - Additionally, every line of the file matching `gadget <name>` emits one
//!   `fixture:gadget:<name>` entity anchored at that line. Content-driven, so
//!   host tests can construct duplicate-locator shapes (the same gadget name
//!   twice in one file, or shared across files) without extra env knobs
//!   (clarion-b19fe90c3e).

use std::collections::BTreeMap;
use std::io::{BufReader, Write};

/// True when the named environment variable is set to exactly `"1"`.
///
/// Misbehaviour switches for hardening tests follow the pattern of the
/// existing `LOOMWEAVE_FIXTURE_EXCEED_RLIMIT_AS` gate: checked at the
/// dispatch site, opt-in only, and inert in every normal test run.
fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1")
}

/// Park forever without consuming CPU (hang simulation). The host's watchdog
/// must kill us; sleeping in a loop survives spurious wakeups.
fn hang_forever() -> ! {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

use loomweave_core::plugin::limits::ContentLengthCeiling;
use loomweave_core::plugin::transport::{Frame, read_frame, write_frame};
use loomweave_core::plugin::{
    AnalyzeFileFinding, AnalyzeFileParams, AnalyzeFileResult, AnalyzeFileStats, InitializeResult,
    JsonRpcVersion, ProtocolError, ResponseEnvelope, ResponsePayload, ShutdownResult,
};
use serde_json::Value;

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    loop {
        // Use the ADR-021 default (8 MiB) so this fixture has the same
        // ceiling a real plugin sees. `unbounded()` is now `#[cfg(test)]`
        // only — production code must name an explicit byte limit.
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

        // Request — extract id.
        let Some(id) = raw.get("id").and_then(serde_json::Value::as_i64) else {
            std::process::exit(1)
        };

        match method.as_str() {
            "initialize" => {
                // Hang *before* responding: simulates a plugin that wedges
                // during whole-repo work inside `initialize` (the Rust
                // plugin's symbol-table build runs there). The host's
                // handshake deadline must kill us.
                if env_flag("LOOMWEAVE_FIXTURE_HANG_AT_INITIALIZE") {
                    hang_forever();
                }
                // Protocol refusal: reply to `initialize` with a JSON-RPC
                // ERROR response, then stay alive (parked, not exited). The
                // host must kill+reap us itself — and its own SIGKILL must
                // not be classified as an OOM event (clarion-371efa3e07).
                if env_flag("LOOMWEAVE_FIXTURE_REFUSE_HANDSHAKE") {
                    send_error(&mut writer, id, -32600, "fixture refuses handshake");
                    hang_forever();
                }
                let result = InitializeResult {
                    name: "loomweave-plugin-fixture".to_owned(),
                    version: "0.1.0".to_owned(),
                    ontology_version: "0.1.0".to_owned(),
                    capabilities: serde_json::json!({}),
                };
                send_result(&mut writer, id, serde_json::to_value(result).unwrap());
            }
            "analyze_file" => {
                // CPU-spin before responding: simulates a busy-looping
                // plugin (e.g. a pathological parse). The host's per-file
                // watchdog must kill us — RLIMIT/breaker never fire because
                // we neither allocate nor crash.
                if env_flag("LOOMWEAVE_FIXTURE_SPIN_AT_ANALYZE") {
                    loop {
                        std::hint::spin_loop();
                    }
                }
                // SIGABRT before responding: the real stack-overflow
                // signature (Rust guard page → abort → signal 6).
                if env_flag("LOOMWEAVE_FIXTURE_ABORT_AT_ANALYZE") {
                    std::process::abort();
                }
                if std::env::var_os("LOOMWEAVE_FIXTURE_EXCEED_RLIMIT_AS").is_some() {
                    #[cfg(unix)]
                    exceed_rlimit_as();
                    #[cfg(not(unix))]
                    std::process::exit(1);
                }

                // Extract the file_path from params.
                let file_path = raw
                    .get("params")
                    .and_then(|p| p.get("file_path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned();

                let params: AnalyzeFileParams = match serde_json::from_value(
                    raw.get("params").cloned().unwrap_or(serde_json::json!({})),
                ) {
                    Ok(p) => p,
                    Err(_) => std::process::exit(1),
                };
                let _ = params; // we already extracted file_path

                let widget = serde_json::json!({
                    "id": "fixture:widget:demo.sample",
                    "kind": "widget",
                    "qualified_name": "demo.sample",
                    "source": {
                        "file_path": file_path
                    }
                });
                let mut entities = vec![widget];
                // Misbehaviour switch (clarion-b19fe90c3e): re-emit the
                // file_scope widget a second time from the SAME file — the
                // plugin-bug shape of a duplicate locator.
                if env_flag("LOOMWEAVE_FIXTURE_DUPLICATE_WIDGET") {
                    let duplicate = entities[0].clone();
                    entities.push(duplicate);
                }
                // Content-driven gadget emission: `gadget <name>` lines each
                // emit one `fixture:gadget:<name>` entity anchored at that
                // line. The line range gives the host a content hash, so the
                // entity participates in the prior index (incremental-run
                // shapes need that).
                entities.extend(gadget_entities(&file_path));
                let result = AnalyzeFileResult {
                    entities,
                    edges: vec![],
                    stats: AnalyzeFileStats::default(),
                    findings: forged_anchor_findings(),
                };
                send_result(&mut writer, id, serde_json::to_value(result).unwrap());
            }
            "shutdown" => {
                // Hang *before* responding: simulates a plugin that goes
                // silent at shutdown after the work completed. The host's
                // shutdown deadline must kill us without failing the run.
                if env_flag("LOOMWEAVE_FIXTURE_HANG_AT_SHUTDOWN") {
                    hang_forever();
                }
                let result = ShutdownResult {};
                send_result(&mut writer, id, serde_json::to_value(result).unwrap());
            }
            _ => std::process::exit(1),
        }
    }
}

/// Opt-in trust-boundary probe: when `LOOMWEAVE_FIXTURE_FINDING_FORGED_ANCHOR`
/// is set, emit one plugin finding whose metadata carries a FORGED
/// host-reserved `anchor_entity_id` naming an entity that does not exist. The
/// host MUST strip this key at the plugin boundary (`validate_plugin_finding`)
/// so it cannot override the trusted file anchor; otherwise the finding's
/// `entity_id` FK insert hard-fails the whole analyze run. Inert in normal runs.
fn forged_anchor_findings() -> Vec<AnalyzeFileFinding> {
    if !env_flag("LOOMWEAVE_FIXTURE_FINDING_FORGED_ANCHOR") {
        return Vec::new();
    }
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "anchor_entity_id".to_owned(),
        Value::String("fixture:gadget:forged.ghost.nonexistent".to_owned()),
    );
    vec![AnalyzeFileFinding {
        subcode: "LMWV-FIXTURE-FORGED-ANCHOR".to_owned(),
        severity: Some("warning".to_owned()),
        message: "fixture emitted a finding with a forged anchor_entity_id".to_owned(),
        metadata,
    }]
}

/// Parse `gadget <name>` lines out of the analysed file and build one
/// `fixture:gadget:<name>` raw entity per line, carrying a one-line
/// `source_range` so the host derives a content hash for it. An unreadable
/// file yields no gadgets (the hardcoded widget still flows).
fn gadget_entities(file_path: &str) -> Vec<Value> {
    let Ok(content) = std::fs::read_to_string(file_path) else {
        return Vec::new();
    };
    let mut gadgets = Vec::new();
    for (index, line) in content.lines().enumerate() {
        let Some(rest) = line.strip_prefix("gadget ") else {
            continue;
        };
        let Some(name) = rest.split_whitespace().next() else {
            continue;
        };
        let line_number = index + 1;
        gadgets.push(serde_json::json!({
            "id": format!("fixture:gadget:{name}"),
            "kind": "gadget",
            "qualified_name": name,
            "source": {
                "file_path": file_path,
                "source_range": { "start_line": line_number, "end_line": line_number }
            }
        }));
    }
    gadgets
}

fn send_error(writer: &mut impl Write, id: i64, code: i64, message: &str) {
    let env = ResponseEnvelope {
        jsonrpc: JsonRpcVersion,
        id,
        payload: ResponsePayload::Error(ProtocolError {
            code,
            message: message.to_owned(),
            data: None,
        }),
    };
    let body = serde_json::to_vec(&env).expect("serialise error response");
    let frame = Frame { body };
    write_frame(writer, &frame).expect("write frame");
    writer.flush().expect("flush");
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

#[cfg(unix)]
fn exceed_rlimit_as() -> ! {
    use std::num::NonZeroUsize;

    const FIRST_REQUEST_BYTES: usize = 128 * 1024 * 1024;
    let mut mappings = Vec::new();
    mappings
        .try_reserve_exact(1024)
        .expect("reserve mapping handles before memory pressure");
    let mut request_bytes = FIRST_REQUEST_BYTES;
    loop {
        let length = NonZeroUsize::new(request_bytes).expect("non-zero request");
        // SAFETY: This fixture intentionally asks the kernel for anonymous
        // address space after the host has applied RLIMIT_AS. The mapping is
        // never dereferenced; it is only held so successful probes continue to
        // count against the child process until the next probe fails.
        let mapped = {
            #[allow(unsafe_code)]
            unsafe {
                nix::sys::mman::mmap_anonymous(
                    None,
                    length,
                    nix::sys::mman::ProtFlags::PROT_NONE,
                    nix::sys::mman::MapFlags::MAP_PRIVATE,
                )
            }
        };
        match mapped {
            Ok(ptr) => {
                mappings.push(ptr);
                let next_request = request_bytes.saturating_mul(2);
                if next_request == request_bytes {
                    terminate_after_rlimit_failure();
                }
                request_bytes = next_request;
            }
            Err(_) => {
                terminate_after_rlimit_failure();
            }
        }
    }
}

#[cfg(unix)]
fn terminate_after_rlimit_failure() -> ! {
    let _ = nix::sys::signal::kill(nix::unistd::Pid::this(), nix::sys::signal::Signal::SIGKILL);
    std::process::abort();
}
