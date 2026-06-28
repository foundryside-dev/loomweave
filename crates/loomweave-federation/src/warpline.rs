//! Warpline churn-count consumer for Loomweave.
//!
//! Loomweave's `entity_high_churn_list` / `entity_recent_change_list` MCP
//! surfaces were dead-by-design: loomweave does not populate `git_churn_count`
//! in v1.0 and, by the seam's HARD RULE, retains no cross-run history. Warpline
//! is the federation's temporal authority that *does* hold per-entity change
//! counts. This module is the read-time consumer of Warpline's FROZEN churn
//! read `warpline_entity_churn_count_get` (`warpline.entity_churn_count.v1`,
//! 2026-06-13 interface lock §1A / GV-LW-2).
//!
//! Discipline (all load-bearing, from the lock):
//! - **READ-ONLY / DEPENDENCY-SINK.** Nothing flows loomweave→warpline here.
//!   loomweave asks for counts, joins at read time, and stores NOTHING — no new
//!   table, no retained warpline fact (§5 HARD RULE).
//! - **ENRICH-ONLY HONEST-DEGRADE.** Warpline absent/disabled/unreachable →
//!   the consumer reports honest-unavailable with a reason; it never breaks
//!   loomweave's core flow and never reads absence as a clean/empty answer
//!   (§1C, §2 ENRICH-ONLY).
//! - **SEI-KEYED, LOCATOR FALLBACK.** Refs are keyed on the SEI when loomweave
//!   has resolved one, else on the entity locator (the entity id). A
//!   never-observed ref returns `churn_count: 0` from warpline — a real,
//!   complete answer, not an error (lock §1A "Keying").
//!
//! Transport: Warpline is an MCP-stdio member (no HTTP read API), so it is
//! launched as a subprocess and driven over MCP stdio. Unlike Loomweave's own
//! language plugins (Content-Length framed, ADR-002), `warpline-mcp` speaks
//! **newline-delimited** JSON-RPC — one compact JSON object per line, one
//! response line per request line. This client frames to match (an earlier
//! Content-Length copy of the Filigree path hung the read against warpline's
//! line transport). The whole exchange runs in a worker thread bounded by a
//! per-call timeout: a warpline child that accepts the connection and never
//! answers is killed, and the surface degrades to `warpline-unreachable` rather
//! than hanging.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::config::WarplineConfig;

/// The endorsed FROZEN tool name (lock §1A). The short shim is `churn`.
pub const WARPLINE_CHURN_TOOL: &str = "warpline_entity_churn_count_get";
/// The frozen contract URI carried in warpline's success envelope `schema`.
pub const WARPLINE_CHURN_SCHEMA: &str = "warpline.entity_churn_count.v1";

/// A single entity ref Loomweave sends to warpline. The frozen ref shape is
/// `{kind, value}` (lock "Entity references and SEI keying"). Loomweave emits
/// `kind: "sei"` when it holds a resolved SEI, else `kind: "locator"` carrying
/// the entity id (which *is* a loomweave locator).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WarplineEntityRef {
    pub kind: &'static str,
    pub value: String,
}

impl WarplineEntityRef {
    /// SEI ref when `sei` is present and non-blank, else a locator ref keyed on
    /// the entity id. Never drops a candidate — an unresolved entity is sent as
    /// a locator and warpline answers `churn_count: 0` if it has never observed
    /// it.
    #[must_use]
    pub fn for_entity(entity_id: &str, sei: Option<&str>) -> Self {
        match sei.map(str::trim).filter(|s| !s.is_empty()) {
            Some(sei) => Self {
                kind: "sei",
                value: sei.to_owned(),
            },
            None => Self {
                kind: "locator",
                value: entity_id.to_owned(),
            },
        }
    }
}

/// One `data.items[]` row from the frozen `warpline.entity_churn_count.v1`
/// output (lock §1A). Unknown fields are ignored so warpline can grow the row
/// without breaking this read.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChurnItem {
    /// Echoed entity keys. Carries both `sei` (null when warpline has not
    /// resolved one) and `locator`.
    #[serde(default)]
    pub entity: ChurnEntity,
    /// Count of change events. A never-observed ref is `0` (not omitted, not an
    /// error) — the GV-LW-2 invariant.
    pub churn_count: i64,
    #[serde(default)]
    pub first_changed_at: Option<String>,
    #[serde(default)]
    pub last_changed_at: Option<String>,
    #[serde(default)]
    pub last_actor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct ChurnEntity {
    #[serde(default)]
    pub sei: Option<String>,
    #[serde(default)]
    pub locator: Option<String>,
}

/// The `data` payload of the frozen churn envelope (`data.items` is the part
/// loomweave joins on; `data.overflow` discloses truncation).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChurnData {
    #[serde(default)]
    pub items: Vec<ChurnItem>,
    /// Warpline's overflow carrier — present when the read was bounded.
    #[serde(default)]
    pub overflow: Option<ChurnOverflow>,
}

/// The `data.overflow` carrier from the frozen envelope. Warpline bounds an
/// oversized churn read: it keeps a lead window in-band (`returned` of `total`)
/// and spills the FULL list to `dumped_to`, reporting `reason_class: "partial"`
/// (else `"clean"`). Loomweave reads `reason_class` / `total` / `returned` to
/// DISCLOSE that a ranking is partial — so a truncated-out entity's
/// `churn_count: 0` (warpline *has* a record) is never conflated with a genuine
/// never-observed `0`. Reading `dumped_to` for complete coverage of a scope
/// larger than warpline's in-band cap is a tracked follow-up (deep-pagination).
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct ChurnOverflow {
    #[serde(default)]
    pub reason_class: Option<String>,
    #[serde(default)]
    pub total: Option<i64>,
    #[serde(default)]
    pub returned: Option<i64>,
    #[serde(default)]
    pub dumped_to: Option<String>,
}

/// The full FROZEN success envelope warpline returns
/// (`{schema, ok, query, data, warnings, …}`). Loomweave reads `data.items`;
/// the rest is tolerated so the parse pins the *wire* shape, not a convenient
/// subset (GV-LW-2 is asserted against this envelope).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChurnCountResponse {
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub ok: Option<bool>,
    pub data: ChurnData,
}

impl ChurnCountResponse {
    /// Index the returned counts by both SEI and locator so the caller can look
    /// up a count for an entity regardless of which key it was sent under. The
    /// value is the full [`ChurnItem`] (count + first/last/actor) — the recency
    /// surface needs `last_changed_at`.
    #[must_use]
    pub fn index_by_key(&self) -> HashMap<String, &ChurnItem> {
        let mut by_key = HashMap::new();
        for item in &self.data.items {
            if let Some(sei) = item.entity.sei.as_deref().filter(|s| !s.is_empty()) {
                by_key.insert(sei.to_owned(), item);
            }
            if let Some(locator) = item.entity.locator.as_deref().filter(|s| !s.is_empty()) {
                by_key.insert(locator.to_owned(), item);
            }
        }
        by_key
    }

    /// When warpline truncated the churn read to an in-band lead (overflow
    /// `reason_class: "partial"`), the `(total, counted)` pair: `total` refs were
    /// requested but only `counted` carry real counts in-band — the rest are
    /// absent from `data.items`, so a join reads them as `0`. `None` when the
    /// answer is complete (`clean` / no overflow), in which case every `0` is a
    /// genuine never-observed count. The caller discloses the partial case so the
    /// two kinds of `0` are not conflated.
    #[must_use]
    pub fn overflow_partial(&self) -> Option<(i64, i64)> {
        let overflow = self.data.overflow.as_ref()?;
        if overflow.reason_class.as_deref() != Some("partial") {
            return None;
        }
        let items_len = i64::try_from(self.data.items.len()).unwrap_or(i64::MAX);
        let counted = overflow.returned.unwrap_or(items_len);
        let total = overflow.total.unwrap_or(counted);
        Some((total, counted))
    }

    /// Count of returned items that are KEYING MISSES — refs warpline could not
    /// resolve, returned with `churn_count: 0`. Distinguishable from a genuine
    /// never-observed `0`: warpline echoes a non-null `locator` for a resolved
    /// entity (the `entity_keys.locator` column is NOT NULL), but `locator: null`
    /// for an unresolved **SEI** ref (producer `commands.py`: a resolve miss on a
    /// sei-kind ref sets `{sei: <sent>, locator: null}`). So `locator.is_none()`
    /// flags a ref whose `0` means "warpline has no key for this entity" (its real
    /// churn is unknown, not zero) — the loomweave↔warpline keying/dialect gap.
    ///
    /// Caveat: only catches SEI-keyed misses. A *locator*-kind miss echoes the
    /// sent value back as `locator`, so it is indistinguishable from a genuine
    /// never-observed `0` here — the caller's disclosure is bounded accordingly.
    #[must_use]
    pub fn unresolved_ref_count(&self) -> usize {
        self.data
            .items
            .iter()
            .filter(|item| item.entity.locator.is_none())
            .count()
    }
}

/// Parse the FROZEN churn envelope body. Pins the wire contract: a body that is
/// not `{…, "data": {"items": [...]}, …}` is a contract error, surfaced so the
/// caller degrades the surface to honest-unavailable rather than fabricating an
/// empty ranking.
///
/// # Errors
/// Returns [`WarplineContractError`] when the body is not valid frozen-envelope
/// JSON.
pub fn parse_churn_count_response(body: &str) -> Result<ChurnCountResponse, WarplineContractError> {
    serde_json::from_str(body).map_err(WarplineContractError::from)
}

#[derive(Debug, Error)]
pub enum WarplineContractError {
    #[error("invalid Warpline churn response: {0}")]
    InvalidResponse(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum WarplineClientError {
    #[error("run Warpline MCP tool {tool}: {message}")]
    McpTool { tool: String, message: String },

    /// Warpline returned its FROZEN `warpline.error.v1` error envelope (e.g.
    /// `invalid_changed_refs` for an unrecognised ref shape).
    #[error("Warpline returned an error for {tool}: {message}")]
    WarplineError { tool: String, message: String },

    #[error(transparent)]
    Contract(#[from] WarplineContractError),
}

/// The read-only Warpline seam Loomweave depends on. ONE method — the churn
/// read. No timeline/blast-radius methods: an unused method would be
/// dead-by-design (the very thing this seam exists to cure). The default impl
/// reports the read unavailable so a test double / read-only deployment opts in
/// explicitly and cannot accidentally pretend a count was returned.
pub trait WarplineLookup: Send + Sync {
    /// Per-entity change counts for `entity_refs` over an optional `window`,
    /// keyed by SEI (or locator). `window` is the frozen
    /// `{since, until, rev_range}` object; `None` means the all-time count.
    /// Returns the full frozen envelope so the caller can index counts by key.
    ///
    /// # Errors
    /// Returns [`WarplineClientError`] on transport failure, a warpline error
    /// envelope, or an unparseable body — every one of which the caller treats
    /// as honest-unavailable, never as a clean/empty ranking.
    fn entity_churn_counts(
        &self,
        _entity_refs: &[WarplineEntityRef],
        _window: Option<&serde_json::Value>,
    ) -> Result<ChurnCountResponse, WarplineClientError> {
        Err(WarplineClientError::McpTool {
            tool: WARPLINE_CHURN_TOOL.to_owned(),
            message: "Warpline churn read is unavailable (no client configured)".to_owned(),
        })
    }
}

/// MCP-stdio client for Warpline's churn read. Construction is gated on
/// `config.enabled`; an absent client (`None`) is the honest-degrade default.
#[derive(Debug, Clone)]
pub struct WarplineMcpClient {
    /// The resolved launch command (program, args) for `warpline-mcp`. Resolved
    /// once at construction from the env override / `warpline mcp` shim so the
    /// transport path stays env-free and unit-testable (a test injects a fake
    /// newline-MCP server here directly).
    command: (String, Vec<String>),
    /// The repo root sent as the required `repo` argument and used as the
    /// subprocess working directory.
    project_root: Option<PathBuf>,
    /// Per-call round-trip bound; a hung warpline is killed at this deadline.
    timeout: Duration,
}

impl WarplineMcpClient {
    /// Build a client from config, or `None` when the seam is disabled. The
    /// returned client is wired to reach warpline as a subprocess rooted at
    /// `project_root`.
    #[must_use]
    pub fn from_config(config: &WarplineConfig, project_root: Option<&Path>) -> Option<Self> {
        if !config.enabled {
            return None;
        }
        Some(Self {
            command: resolve_warpline_mcp_command(project_root),
            project_root: project_root.map(Path::to_path_buf),
            // Floor a degenerate `0` to 1s so the knob can never mean "never
            // wait" (which would make every call instantly time out).
            timeout: Duration::from_secs(config.timeout_seconds.max(1)),
        })
    }

    fn run_churn_tool(
        &self,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, WarplineClientError> {
        let tool = WARPLINE_CHURN_TOOL;
        let (program, args) = &self.command;
        let mut child = Command::new(program)
            .args(args)
            // stderr is deliberately discarded: this client never drains it, so
            // a large warpline traceback that filled a piped stderr (64 KiB)
            // would block warpline mid-write. Diagnostics surface through the
            // honest-degrade reason, not warpline's stderr.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .current_dir(
                self.project_root
                    .as_deref()
                    .unwrap_or_else(|| Path::new(".")),
            )
            .spawn()
            .map_err(|err| mcp_tool_error(tool, &format!("spawn {program}: {err}")))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| mcp_tool_error(tool, "child stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| mcp_tool_error(tool, "child stdout unavailable"))?;

        // MCP handshake + the one churn call. `warpline-mcp` is a stateless
        // per-line dispatcher (it does not require the handshake), but sending
        // it keeps us a correct MCP client; the reader skips the init result and
        // the notification's spurious `id: null` error by id.
        let init = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "loomweave-init",
            "method": "initialize",
            "params": {
                // A protocol version warpline advertises (2024-11-05 / 2025-03-26);
                // warpline negotiates down rather than rejecting, but match anyway.
                "protocolVersion": "2025-03-26",
                "capabilities": {},
                "clientInfo": { "name": "loomweave", "version": env!("CARGO_PKG_VERSION") }
            }
        });
        let initialized = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        let call = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "loomweave-call",
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments }
        });

        // Drive the exchange on a worker thread so the blocking write+read is
        // bounded by `recv_timeout`. On timeout we kill the child, which closes
        // its pipes and unblocks the worker. The handshake responses are tiny
        // (well under a pipe buffer), so warpline never blocks on stdout-write
        // while we are still writing stdin — no write/read deadlock.
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let outcome = (|| -> Result<serde_json::Value, WarplineClientError> {
                write_json_line(&mut stdin, &init, tool)?;
                write_json_line(&mut stdin, &initialized, tool)?;
                write_json_line(&mut stdin, &call, tool)?;
                stdin
                    .flush()
                    .map_err(|err| mcp_tool_error(tool, &format!("flush warpline stdin: {err}")))?;
                // EOF on stdin ends warpline's read loop so it exits cleanly.
                drop(stdin);
                let mut reader = BufReader::new(stdout);
                read_response_for_id(&mut reader, "loomweave-call", tool)
            })();
            let _ = tx.send(outcome);
        });

        match rx.recv_timeout(self.timeout) {
            Ok(outcome) => {
                let _ = child.wait();
                let _ = worker.join();
                envelope_from_response(&outcome?, tool)
            }
            Err(RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = worker.join();
                Err(mcp_tool_error(
                    tool,
                    &format!(
                        "warpline did not respond within {}s",
                        self.timeout.as_secs()
                    ),
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = child.wait();
                Err(mcp_tool_error(
                    tool,
                    "warpline worker thread disconnected before responding",
                ))
            }
        }
    }
}

impl WarplineLookup for WarplineMcpClient {
    fn entity_churn_counts(
        &self,
        entity_refs: &[WarplineEntityRef],
        window: Option<&serde_json::Value>,
    ) -> Result<ChurnCountResponse, WarplineClientError> {
        let mut arguments = serde_json::json!({
            "entity_refs": entity_refs,
            // Ask warpline to rank by count, descending — loomweave re-ranks its
            // own scoped set from the returned counts regardless.
            "sort_by": "churn_count",
            "sort_order": "desc",
        });
        if let Some(obj) = arguments.as_object_mut() {
            // `repo` is REQUIRED by warpline (`_repo_arg`); warpline keys its
            // store by it, so it must match the repo warpline indexed (the
            // subprocess working dir). Omitting it made every call error;
            // sending the wrong path silently resolves every ref to 0.
            if let Some(root) = self.project_root.as_deref() {
                obj.insert(
                    "repo".to_owned(),
                    serde_json::json!(root.display().to_string()),
                );
            }
            // Page the whole ref set in-band: warpline defaults `limit` to 100,
            // so without this it would echo counts for only the top 100 refs by
            // churn and the join would read every other candidate as `0`. `.max(1)`
            // dodges warpline's `limit <= 0` rejection on an empty ref set. This
            // covers the page cap; warpline's separate overflow cap (in-band lead)
            // still bounds very large scopes — disclosed via `overflow_partial`.
            obj.insert(
                "limit".to_owned(),
                serde_json::json!(entity_refs.len().max(1)),
            );
            if let Some(window) = window {
                obj.insert("window".to_owned(), window.clone());
            }
        }
        // `actor` is deliberately NOT sent: it is not in warpline's frozen churn
        // schema (its `inputSchema` is `additionalProperties: false`), so we omit
        // it. (Warpline does not enforce that at runtime — it ignores unknown
        // params — but the contract is what we conform to.)
        let value = self.run_churn_tool(&arguments)?;
        let body = value.to_string();
        parse_churn_count_response(&body).map_err(WarplineClientError::Contract)
    }
}

/// Build a transport-level [`WarplineClientError::McpTool`] (every variant the
/// caller treats as honest-unavailable).
fn mcp_tool_error(tool: &str, message: &str) -> WarplineClientError {
    WarplineClientError::McpTool {
        tool: tool.to_owned(),
        message: message.to_owned(),
    }
}

/// Write one newline-delimited JSON-RPC message: compact JSON (no embedded
/// newlines) followed by `\n`, the framing `warpline-mcp` reads line-by-line.
fn write_json_line(
    writer: &mut impl Write,
    value: &serde_json::Value,
    tool: &str,
) -> Result<(), WarplineClientError> {
    let mut body = serde_json::to_vec(value)
        .map_err(|err| mcp_tool_error(tool, &format!("serialize MCP request: {err}")))?;
    body.push(b'\n');
    writer
        .write_all(&body)
        .map_err(|err| mcp_tool_error(tool, &format!("write MCP request: {err}")))
}

/// Read newline-delimited JSON-RPC responses until one carries `expected_id`,
/// skipping the init result and the notification's `id: null` error. EOF before
/// a match is a transport fault (honest-unavailable, never a clean empty).
fn read_response_for_id(
    reader: &mut impl BufRead,
    expected_id: &str,
    tool: &str,
) -> Result<serde_json::Value, WarplineClientError> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| mcp_tool_error(tool, &format!("read MCP response: {err}")))?;
        if read == 0 {
            return Err(mcp_tool_error(
                tool,
                "warpline closed its output before answering the churn call",
            ));
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|err| mcp_tool_error(tool, &format!("parse MCP response line: {err}")))?;
        if value.get("id").and_then(serde_json::Value::as_str) == Some(expected_id) {
            return Ok(value);
        }
        // A non-matching id (init result, the notification's id:null error) is
        // skipped; keep reading for our call's response.
    }
}

/// Pull the FROZEN churn envelope out of a `tools/call` JSON-RPC response: a
/// JSON-RPC `error` degrades; otherwise prefer `result.structuredContent` (the
/// envelope as an object) and fall back to `result.content[0].text` (the same
/// envelope as a JSON string) — `warpline-mcp` returns both.
fn envelope_from_response(
    response: &serde_json::Value,
    tool: &str,
) -> Result<serde_json::Value, WarplineClientError> {
    if let Some(error) = response.get("error").filter(|err| !err.is_null()) {
        return Err(WarplineClientError::WarplineError {
            tool: tool.to_owned(),
            message: error.to_string(),
        });
    }
    let result = response
        .get("result")
        .ok_or_else(|| mcp_tool_error(tool, &format!("response missing result: {response}")))?;
    if let Some(structured) = result.get("structuredContent").filter(|v| !v.is_null()) {
        return Ok(structured.clone());
    }
    if let Some(text) = result
        .get("content")
        .and_then(serde_json::Value::as_array)
        .and_then(|content| content.first())
        .and_then(|item| item.get("text"))
        .and_then(serde_json::Value::as_str)
    {
        return serde_json::from_str(text)
            .map_err(|err| WarplineClientError::Contract(WarplineContractError::from(err)));
    }
    Err(mcp_tool_error(
        tool,
        &format!("response result has neither structuredContent nor content[0].text: {response}"),
    ))
}

/// Resolve the command that launches warpline's MCP stdio server. Env override
/// `LOOMWEAVE_WARPLINE_MCP_COMMAND` (with a `{project}` placeholder) wins; else
/// the `warpline-mcp` binary.
///
/// NOTE: the launcher is the standalone `warpline-mcp` executable, NOT
/// `warpline mcp` — warpline's CLI has no `mcp` subcommand, so `warpline mcp`
/// exits with a usage error and the write to its (already-closed) stdin fails
/// with a broken pipe. The MCP stdio server only ships as `warpline-mcp`.
fn resolve_warpline_mcp_command(project_root: Option<&Path>) -> (String, Vec<String>) {
    if let Ok(raw) = std::env::var("LOOMWEAVE_WARPLINE_MCP_COMMAND") {
        let mut parts: Vec<String> = raw
            .split_whitespace()
            .map(|part| match project_root {
                Some(root) => part.replace("{project}", &root.display().to_string()),
                None => part.to_owned(),
            })
            .collect();
        if let Some(program) = parts.first().cloned() {
            parts.remove(0);
            return (program, parts);
        }
    }
    ("warpline-mcp".to_owned(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake `warpline-mcp`: a **newline-delimited** JSON-RPC server (exactly
    /// the transport `warpline-mcp` speaks). `argv[1]` selects a mode; `argv[2]`,
    /// when present, is a sidecar path the tool-call arguments are dumped to so a
    /// test can assert what loomweave sent.
    ///
    /// - `both`      → reply with `structuredContent` AND `content[0].text`.
    /// - `text_only` → reply with `content[0].text` only (fallback parse path).
    /// - `hang`      → complete the handshake, then never answer the call.
    ///
    /// It mirrors warpline: a response line per request line, including the
    /// `id: null` error for the `notifications/initialized` notification.
    const FAKE_SERVER_PY: &str = r#"
import sys, json, time

mode = sys.argv[1] if len(sys.argv) > 1 else "both"
sidecar = sys.argv[2] if len(sys.argv) > 2 else None

ENVELOPE = {
    "schema": "warpline.entity_churn_count.v1",
    "ok": True,
    "data": {"items": [
        {"entity": {"sei": "loomweave:eid:aaa", "locator": "python:function:m::alpha"},
         "churn_count": 7, "first_changed_at": "2026-05-01T00:00:00Z",
         "last_changed_at": "2026-06-13T00:00:00Z", "last_actor": "agent:codex"},
        {"entity": {"sei": "loomweave:eid:bbb", "locator": "python:function:m::beta"},
         "churn_count": 2, "first_changed_at": None,
         "last_changed_at": "2026-06-01T00:00:00Z", "last_actor": None},
        {"entity": {"sei": "loomweave:eid:ccc", "locator": "python:function:m::gamma"},
         "churn_count": 0, "first_changed_at": None,
         "last_changed_at": None, "last_actor": None}
    ]}
}

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method = req.get("method")
    rid = req.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2025-03-26",
            "serverInfo": {"name": "fake-warpline", "version": "0"},
            "capabilities": {"tools": {}}}})
    elif method == "tools/call":
        args = (req.get("params") or {}).get("arguments") or {}
        if sidecar:
            with open(sidecar, "w") as f:
                json.dump(args, f)
        if mode == "hang":
            time.sleep(60)
            continue
        result = {"content": [{"type": "text", "text": json.dumps(ENVELOPE, sort_keys=True)}]}
        if mode != "text_only":
            result["structuredContent"] = ENVELOPE
        send({"jsonrpc": "2.0", "id": rid, "result": result})
    elif rid is not None:
        send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "unknown"}})
    else:
        send({"jsonrpc": "2.0", "id": None, "error": {"code": -32601, "message": "unknown method"}})
"#;

    fn write_fake_server(dir: &Path) -> PathBuf {
        let script = dir.join("fake_warpline.py");
        std::fs::write(&script, FAKE_SERVER_PY).expect("write fake warpline server");
        script
    }

    fn fake_client(
        script: &Path,
        mode: &str,
        sidecar: Option<&Path>,
        project_root: &Path,
        timeout_secs: u64,
    ) -> WarplineMcpClient {
        let mut args = vec![script.display().to_string(), mode.to_owned()];
        if let Some(sidecar) = sidecar {
            args.push(sidecar.display().to_string());
        }
        WarplineMcpClient {
            command: ("python3".to_owned(), args),
            project_root: Some(project_root.to_path_buf()),
            timeout: Duration::from_secs(timeout_secs),
        }
    }

    /// The transport regression: over the REAL newline-delimited subprocess
    /// transport (not the injected fake `WarplineLookup`), the churn read
    /// completes, sends the required `repo`, omits the unsupported `actor`, and
    /// parses the frozen envelope. This is the bug the consumer hit: the prior
    /// Content-Length framing hung against warpline's line transport, and the
    /// call omitted `repo` / sent `actor`.
    #[test]
    fn real_transport_sends_repo_omits_actor_and_parses_envelope() {
        let dir = tempfile::tempdir().expect("temp dir");
        let script = write_fake_server(dir.path());
        let sidecar = dir.path().join("args.json");
        let client = fake_client(&script, "both", Some(&sidecar), dir.path(), 10);

        let refs = vec![WarplineEntityRef::for_entity(
            "python:function:m::alpha",
            Some("loomweave:eid:aaa"),
        )];
        let response = client
            .entity_churn_counts(&refs, None)
            .expect("churn read succeeds over the newline-delimited transport");
        assert_eq!(
            response.data.items.len(),
            3,
            "the frozen 3-item envelope round-tripped"
        );

        let args: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).expect("sidecar written"))
                .expect("sidecar JSON");
        assert_eq!(
            args["repo"],
            serde_json::json!(dir.path().display().to_string()),
            "the required repo arg is sent, equal to the project root"
        );
        assert!(
            args.get("actor").is_none(),
            "the unsupported actor param must NOT be sent: {args}"
        );
        assert!(
            args.get("entity_refs").is_some(),
            "entity_refs is forwarded: {args}"
        );
    }

    /// Requirement #4: when warpline returns only the text envelope (no
    /// `structuredContent`), the consumer still parses it via the fallback.
    #[test]
    fn real_transport_parses_text_envelope_when_structured_content_absent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let script = write_fake_server(dir.path());
        let client = fake_client(&script, "text_only", None, dir.path(), 10);

        let refs = vec![WarplineEntityRef::for_entity(
            "python:function:m::alpha",
            Some("loomweave:eid:aaa"),
        )];
        let response = client
            .entity_churn_counts(&refs, None)
            .expect("text-envelope fallback parses");
        assert_eq!(response.data.items.len(), 3);
    }

    /// Requirement #5: a warpline child that completes the handshake then never
    /// answers the call must DEGRADE via the bounded timeout, not hang forever.
    #[test]
    fn real_transport_times_out_instead_of_hanging() {
        let dir = tempfile::tempdir().expect("temp dir");
        let script = write_fake_server(dir.path());
        let client = fake_client(&script, "hang", None, dir.path(), 1);

        let refs = vec![WarplineEntityRef::for_entity(
            "python:function:m::alpha",
            Some("loomweave:eid:aaa"),
        )];
        let start = std::time::Instant::now();
        let err = client
            .entity_churn_counts(&refs, None)
            .expect_err("a hung warpline must error, not hang");
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(15),
            "must return promptly via the timeout (1s), took {elapsed:?}"
        );
        assert!(
            matches!(err, WarplineClientError::McpTool { .. }),
            "a transport-level fault: {err}"
        );
        assert!(
            err.to_string().contains("did not respond"),
            "the timeout reason is surfaced for honest-degrade: {err}"
        );
    }

    /// The recorded FROZEN `warpline.entity_churn_count.v1` envelope used as the
    /// GV-LW-2 producer fixture: 3 refs, two observed (`churn_count >= 1`), one
    /// never-observed (`churn_count: 0`, present, not omitted, not an error).
    const GV_LW_2_FIXTURE: &str = r#"{
      "schema": "warpline.entity_churn_count.v1",
      "ok": true,
      "query": {
        "repo": "/abs/path",
        "tool": "warpline_entity_churn_count_get",
        "arguments": {},
        "filters": {},
        "sort": {"by": "churn_count", "order": "desc"},
        "page": {"limit": 100, "cursor": null}
      },
      "data": {
        "items": [
          {"entity": {"sei": "loomweave:eid:0000000000000000000000000000000a",
                      "locator": "python:function:src/pkg/mod.py::alpha"},
           "churn_count": 7,
           "first_changed_at": "2026-05-01T00:00:00Z",
           "last_changed_at": "2026-06-13T00:00:00Z",
           "last_actor": "agent:codex"},
          {"entity": {"sei": "loomweave:eid:0000000000000000000000000000000b",
                      "locator": "python:function:src/pkg/mod.py::beta"},
           "churn_count": 2,
           "first_changed_at": "2026-05-10T00:00:00Z",
           "last_changed_at": "2026-06-01T00:00:00Z",
           "last_actor": "agent:fable"},
          {"entity": {"sei": "loomweave:eid:0000000000000000000000000000000c",
                      "locator": "python:function:src/pkg/mod.py::gamma"},
           "churn_count": 0,
           "first_changed_at": null,
           "last_changed_at": null,
           "last_actor": null}
        ],
        "window": {"since": null, "until": null, "rev_range": null},
        "page": {"limit": 100, "next_cursor": null, "has_more": false}
      },
      "warnings": [],
      "next_actions": {},
      "enrichment": {"sei": "present"},
      "meta": {"producer": {"tool": "warpline", "version": "0.1.0"},
               "local_only": true, "peer_side_effects": []}
    }"#;

    #[test]
    fn parses_frozen_churn_envelope_shape() {
        // GV-LW-2 producer side: parse the FULL frozen envelope through the real
        // parse path and pin the contract — 3 items, two observed, one zero.
        let parsed = parse_churn_count_response(GV_LW_2_FIXTURE).expect("frozen envelope parses");
        // The producer's `schema` matches the frozen contract URI we pin to.
        assert_eq!(parsed.schema.as_deref(), Some(WARPLINE_CHURN_SCHEMA));
        assert_eq!(parsed.ok, Some(true));
        assert_eq!(
            parsed.data.items.len(),
            3,
            "all 3 refs echoed, none omitted"
        );

        let observed: Vec<i64> = parsed
            .data
            .items
            .iter()
            .filter(|i| i.churn_count >= 1)
            .map(|i| i.churn_count)
            .collect();
        assert_eq!(
            observed.len(),
            2,
            "two observed refs carry churn_count >= 1"
        );

        let gamma = parsed
            .data
            .items
            .iter()
            .find(|i| i.entity.locator.as_deref() == Some("python:function:src/pkg/mod.py::gamma"))
            .expect("the never-observed ref is present, not omitted");
        assert_eq!(
            gamma.churn_count, 0,
            "never-observed ref is 0, not an error"
        );
    }

    #[test]
    fn indexes_counts_by_both_sei_and_locator() {
        let parsed = parse_churn_count_response(GV_LW_2_FIXTURE).unwrap();
        let by_key = parsed.index_by_key();
        // Look up by SEI...
        assert_eq!(
            by_key
                .get("loomweave:eid:0000000000000000000000000000000a")
                .map(|i| i.churn_count),
            Some(7)
        );
        // ...and by locator.
        assert_eq!(
            by_key
                .get("python:function:src/pkg/mod.py::beta")
                .map(|i| i.churn_count),
            Some(2)
        );
    }

    #[test]
    fn ref_keys_on_sei_then_falls_back_to_locator() {
        let with_sei =
            WarplineEntityRef::for_entity("python:function:m::f", Some("loomweave:eid:abc"));
        assert_eq!(with_sei.kind, "sei");
        assert_eq!(with_sei.value, "loomweave:eid:abc");

        let no_sei = WarplineEntityRef::for_entity("python:function:m::f", None);
        assert_eq!(no_sei.kind, "locator");
        assert_eq!(no_sei.value, "python:function:m::f");

        // A blank SEI is treated as absent — locator fallback, never an empty key.
        let blank_sei = WarplineEntityRef::for_entity("python:function:m::f", Some("  "));
        assert_eq!(blank_sei.kind, "locator");
        assert_eq!(blank_sei.value, "python:function:m::f");
    }

    #[test]
    fn default_lookup_reports_unavailable_not_empty() {
        // The honest-degrade default: a `WarplineLookup` with no override does
        // NOT return an empty ranking — it errors, so the caller degrades to
        // honest-unavailable rather than reading absence as "no churn".
        struct Bare;
        impl WarplineLookup for Bare {}
        let err = Bare.entity_churn_counts(&[], None).unwrap_err();
        assert!(matches!(err, WarplineClientError::McpTool { .. }));
    }

    #[test]
    fn disabled_config_yields_no_client() {
        let config = WarplineConfig::default(); // enabled: false
        assert!(WarplineMcpClient::from_config(&config, None).is_none());
        let enabled = WarplineConfig {
            enabled: true,
            ..WarplineConfig::default()
        };
        assert!(WarplineMcpClient::from_config(&enabled, None).is_some());
    }

    /// Regression guard for the headline live-found defect: the default launcher
    /// is the standalone `warpline-mcp` binary, NOT `warpline mcp` (which is not a
    /// warpline subcommand and exits with a usage error → broken pipe). The
    /// transport tests inject the command, so this is the only check that pins the
    /// default resolution. Guarded against the env override leaking in from the
    /// surrounding environment.
    #[test]
    fn default_command_is_warpline_mcp_binary_not_subcommand() {
        if std::env::var_os("LOOMWEAVE_WARPLINE_MCP_COMMAND").is_some() {
            return; // env override active; the default is not under test here
        }
        let (program, args) = resolve_warpline_mcp_command(None);
        assert_eq!(program, "warpline-mcp");
        assert!(
            args.is_empty(),
            "the MCP server takes no subcommand args: {args:?}"
        );
    }

    /// `overflow_partial` distinguishes a TRUNCATED read (warpline kept an in-band
    /// lead and spilled the rest) from a complete `clean` answer — the signal the
    /// consumer uses so a truncated-out `0` is not read as never-observed.
    #[test]
    fn overflow_partial_reports_truncation_only_when_partial() {
        // clean (no overflow) → None: every 0 is a genuine never-observed count.
        let clean = parse_churn_count_response(GV_LW_2_FIXTURE).unwrap();
        assert_eq!(clean.overflow_partial(), None);

        // partial → Some((total, counted)) read from warpline's own carrier.
        let partial = parse_churn_count_response(
            r#"{
              "schema": "warpline.entity_churn_count.v1", "ok": true,
              "data": {
                "items": [{"entity": {"locator": "python:function:m::a"}, "churn_count": 5}],
                "overflow": {"total": 574, "returned": 200,
                             "dumped_to": "/abs/.weft/warpline/overflow/x.json",
                             "reason_class": "partial",
                             "cause": "574 items exceeded the 200-item in-band cap",
                             "fix": "read the full list from the dump"}
              }
            }"#,
        )
        .unwrap();
        assert_eq!(partial.overflow_partial(), Some((574, 200)));
    }

    /// `unresolved_ref_count` flags items warpline could not key-match (null
    /// locator) and leaves genuine never-observed items (resolved, non-null
    /// locator, count 0) alone — the two kinds of `0` the consumer must not
    /// conflate.
    #[test]
    fn unresolved_ref_count_flags_null_locator_only() {
        // GV-LW-2 fixture: all 3 items carry a locator (gamma is a genuine
        // never-observed 0, resolved) → zero unresolved.
        let resolved = parse_churn_count_response(GV_LW_2_FIXTURE).unwrap();
        assert_eq!(resolved.unresolved_ref_count(), 0);

        // A SEI-ref miss: warpline echoes the sei but a null locator + count 0.
        let with_miss = parse_churn_count_response(
            r#"{
              "schema": "warpline.entity_churn_count.v1", "ok": true,
              "data": {"items": [
                {"entity": {"sei": "loomweave:eid:hit", "locator": "python:function:m::a"},
                 "churn_count": 4},
                {"entity": {"sei": "loomweave:eid:miss", "locator": null},
                 "churn_count": 0},
                {"entity": {"sei": "loomweave:eid:miss2", "locator": null},
                 "churn_count": 0}
              ]}
            }"#,
        )
        .unwrap();
        assert_eq!(with_miss.unresolved_ref_count(), 2);
    }
}
