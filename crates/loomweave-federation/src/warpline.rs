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
//! launched as a subprocess and driven over MCP stdio — the same mechanism the
//! Filigree MCP-tool calls use (`filigree::run_mcp_tool`). Kept self-contained
//! here rather than sharing filigree's private frame helpers.

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use loomweave_core::plugin::{ContentLengthCeiling, Frame, read_frame, write_frame};
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
/// loomweave joins on).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ChurnData {
    #[serde(default)]
    pub items: Vec<ChurnItem>,
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
    actor: String,
    project_root: Option<PathBuf>,
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
            actor: config.actor.clone(),
            project_root: project_root.map(Path::to_path_buf),
        })
    }

    fn run_churn_tool(
        &self,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, WarplineClientError> {
        let tool = WARPLINE_CHURN_TOOL;
        let (program, args) = resolve_warpline_mcp_command(self.project_root.as_deref());
        let mut child = Command::new(&program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(
                self.project_root
                    .as_deref()
                    .unwrap_or_else(|| Path::new(".")),
            )
            .spawn()
            .map_err(|err| WarplineClientError::McpTool {
                tool: tool.to_owned(),
                message: format!("spawn {program}: {err}"),
            })?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| WarplineClientError::McpTool {
                tool: tool.to_owned(),
                message: "child stdin unavailable".to_owned(),
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| WarplineClientError::McpTool {
                tool: tool.to_owned(),
                message: "child stdout unavailable".to_owned(),
            })?;
        let mut stdout = BufReader::new(stdout);

        write_mcp_frame(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "loomweave-init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": { "name": "loomweave", "version": env!("CARGO_PKG_VERSION") }
                }
            }),
            tool,
        )?;
        let _ = read_mcp_frame(&mut stdout, "loomweave-init", tool)?;
        write_mcp_frame(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
            tool,
        )?;
        write_mcp_frame(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "loomweave-call",
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }),
            tool,
        )?;
        drop(stdin);

        let response = read_mcp_frame(&mut stdout, "loomweave-call", tool)?;
        let _ = child.wait();
        if let Some(error) = response.get("error") {
            return Err(WarplineClientError::McpTool {
                tool: tool.to_owned(),
                message: error.to_string(),
            });
        }
        let text = response
            .get("result")
            .and_then(|result| result.get("content"))
            .and_then(serde_json::Value::as_array)
            .and_then(|content| content.first())
            .and_then(|item| item.get("text"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| WarplineClientError::McpTool {
                tool: tool.to_owned(),
                message: format!("missing result.content[0].text in response {response}"),
            })?;
        let parsed: serde_json::Value = serde_json::from_str(text)
            .map_err(|err| WarplineClientError::Contract(WarplineContractError::from(err)))?;
        // A frozen `warpline.error.v1` body (or any `{ "error": … }`) is an
        // honest "could not answer", surfaced so the caller degrades.
        if let Some(error) = parsed.get("error") {
            return Err(WarplineClientError::WarplineError {
                tool: tool.to_owned(),
                message: error.to_string(),
            });
        }
        Ok(parsed)
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
            "actor": self.actor.clone(),
        });
        if let (Some(window), Some(obj)) = (window, arguments.as_object_mut()) {
            obj.insert("window".to_owned(), window.clone());
        }
        // NOTE (known limitation): there is no per-call timeout. The subprocess
        // round-trip is short-lived in practice, but a warpline child that
        // accepts the connection and never responds would block this read. A
        // `wait_timeout` wrapper is a tracked follow-up (matches the Filigree MCP
        // path's current behaviour); a config knob is deliberately NOT advertised
        // until it is honoured (input-affordances-are-promises).
        let value = self.run_churn_tool(&arguments)?;
        let body = value.to_string();
        parse_churn_count_response(&body).map_err(WarplineClientError::Contract)
    }
}

fn write_mcp_frame(
    writer: &mut impl Write,
    value: &serde_json::Value,
    tool: &str,
) -> Result<(), WarplineClientError> {
    let body = serde_json::to_vec(value).map_err(|err| WarplineClientError::McpTool {
        tool: tool.to_owned(),
        message: format!("serialize MCP request: {err}"),
    })?;
    write_frame(writer, &Frame { body }).map_err(|err| WarplineClientError::McpTool {
        tool: tool.to_owned(),
        message: format!("write MCP frame: {err}"),
    })
}

fn read_mcp_frame(
    reader: &mut impl std::io::BufRead,
    expected_id: &str,
    tool: &str,
) -> Result<serde_json::Value, WarplineClientError> {
    loop {
        let frame = read_frame(reader, ContentLengthCeiling::DEFAULT).map_err(|err| {
            WarplineClientError::McpTool {
                tool: tool.to_owned(),
                message: format!("read MCP frame: {err}"),
            }
        })?;
        let value: serde_json::Value =
            serde_json::from_slice(&frame.body).map_err(|err| WarplineClientError::McpTool {
                tool: tool.to_owned(),
                message: format!("parse MCP response: {err}"),
            })?;
        if value
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| id == expected_id)
        {
            return Ok(value);
        }
    }
}

/// Resolve the command that launches warpline's MCP stdio server. Env override
/// `LOOMWEAVE_WARPLINE_MCP_COMMAND` (with a `{project}` placeholder) wins; else
/// the `warpline mcp` shim.
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
    ("warpline".to_owned(), vec!["mcp".to_owned()])
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
