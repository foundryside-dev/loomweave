//! Minimal test-fixture plugin binary.
//!
//! Speaks the Loomweave JSON-RPC 2.0 protocol over `stdin`/`stdout`. Used by
//! the subprocess integration test (`host_subprocess.rs`) that exercises
//! `PluginHost::spawn`.
//!
//! Fixture identity:
//! - `plugin_id = "fixture"`, kind `"widget"`, rule-ID prefix `"LMWV-FIXTURE-"`
//! - Responds to every `analyze_file` request with one entity:
//!   `id = "fixture:widget:demo.sample"`, `kind = "widget"`,
//!   `qualified_name = "demo.sample"`, `source.file_path` = the path sent in.

use std::io::{BufReader, Write};

use loomweave_core::plugin::limits::ContentLengthCeiling;
use loomweave_core::plugin::transport::{Frame, read_frame, write_frame};
use loomweave_core::plugin::{
    AnalyzeFileParams, AnalyzeFileResult, AnalyzeFileStats, InitializeResult, JsonRpcVersion,
    ResponseEnvelope, ResponsePayload, ShutdownResult,
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
                let result = InitializeResult {
                    name: "loomweave-plugin-fixture".to_owned(),
                    version: "0.1.0".to_owned(),
                    ontology_version: "0.1.0".to_owned(),
                    capabilities: serde_json::json!({}),
                };
                send_result(&mut writer, id, serde_json::to_value(result).unwrap());
            }
            "analyze_file" => {
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

                let entity = serde_json::json!({
                    "id": "fixture:widget:demo.sample",
                    "kind": "widget",
                    "qualified_name": "demo.sample",
                    "source": {
                        "file_path": file_path
                    }
                });
                let result = AnalyzeFileResult {
                    entities: vec![entity],
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
