//! Filigree HTTP/MCP contract helpers for Loomweave MCP.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::FiligreeConfig;
use crate::scan_results::{
    CleanStaleRequest, CleanStaleResponse, ScanResultsRequest, ScanResultsResponse,
    clean_stale_url, parse_clean_stale_response, parse_scan_results_response, scan_results_url,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EntityAssociationsResponse {
    /// `default` so an absent/empty envelope key degrades to an empty list
    /// (enrich-only) rather than hard-failing the whole entity issue-list read.
    #[serde(default)]
    pub associations: Vec<EntityAssociation>,
}

/// The subset of a Filigree issue Loomweave surfaces alongside an
/// entity-association match: enough to render the match without an agent
/// having to call back into Filigree. Sourced from `GET /api/weft/issues/{id}`.
/// Unknown fields in the response are ignored, so Filigree can grow the route
/// without breaking this read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IssueDetail {
    /// The Filigree issue id, carried INSIDE the stub so a consumer acting on
    /// a matched row has the complete (id, title, status) tuple in one place
    /// (weft-4a46553503 / dogfood-4 B9). Deserializes from the route's
    /// `issue_id` field; `default` keeps the read enrich-only against an older
    /// server that omits it (the caller backfills from the association row).
    #[serde(alias = "issue_id", default)]
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: i64,
}

/// Request Loomweave sends to Filigree's observation scratchpad when an agent
/// proposes guidance. This is an observation, not a Loomweave sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservationCreateRequest {
    pub summary: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
    pub priority: i64,
    pub actor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ObservationCreateResponse {
    pub observation_id: String,
}

/// Pending Filigree observation row, as read from `GET /api/weft/observations`
/// or from a test double. Unknown live fields are ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ObservationRecord {
    pub observation_id: String,
    pub summary: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub file_path: String,
    #[serde(default)]
    pub line: Option<i64>,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub actor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EntityAssociation {
    pub issue_id: String,
    /// Opaque Loomweave association key as stored by Filigree. New bindings use
    /// the entity's SEI (`loomweave:eid:*`); legacy rows may still carry the
    /// mutable locator (`{plugin}:{kind}:{qualname}`).
    ///
    /// `alias = "clarion_entity_id"` tolerates the pre-v26 producer field name
    /// (Filigree renamed `clarion_entity_id` → `loomweave_entity_id` in schema
    /// v26 / 3.0.0 with no compat alias), so a pre-v26 server or JSONL export
    /// still deserializes. We deliberately do NOT alias the co-emitted canonical
    /// `entity_id`: the live producer emits BOTH keys with identical values, and
    /// serde rejects the duplicate field slot — aliasing `entity_id` would
    /// hard-fail against the current server. `clarion_entity_id` is safe because
    /// it is never co-emitted alongside `loomweave_entity_id`.
    #[serde(alias = "clarion_entity_id")]
    pub loomweave_entity_id: String,
    pub content_hash_at_attach: String,
    /// `default`: display-only enrichment (never routing/drift logic), so its
    /// absence degrades to an empty string instead of failing the read.
    #[serde(default)]
    pub attached_at: String,
    /// `default`: display-only actor identity; absence degrades, never fails.
    #[serde(default)]
    pub attached_by: String,
}

/// One Wardline finding as Loomweave surfaces it — the subset of Filigree's
/// `ScanFindingWeft` (`GET /api/weft/findings`) used for read-time
/// reconciliation. Unknown fields are ignored so Filigree can grow the row.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct WardlineFinding {
    pub rule_id: String,
    pub message: String,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub line_start: Option<i64>,
    #[serde(default)]
    pub line_end: Option<i64>,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub file_id: Option<String>,
    /// The finding's `metadata` object; `metadata.wardline.qualname` is the
    /// reconciliation key. Defaults to JSON null when absent.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Envelope returned by `GET /api/weft/findings` — the paged list of
/// [`WardlineFinding`] rows Loomweave reconciles against.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WardlineFindingsResponse {
    #[serde(default)]
    pub items: Vec<WardlineFinding>,
    /// True when more findings pages follow. Loomweave does not page the findings
    /// list (the offset param is unpinned in the federation contract); when this
    /// is true the first page is an incomplete view, so the caller fails closed
    /// to `unavailable` rather than silently undercounting the file's findings.
    #[serde(default)]
    pub has_more: bool,
}

/// One row of `GET /api/weft/files` — only the fields needed to map a path to
/// Filigree's `file_id`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WeftFileRecord {
    pub file_id: String,
    pub path: String,
}

/// Envelope returned by `GET /api/weft/files` — the paged list of
/// [`WeftFileRecord`] rows Loomweave uses to map a path to a `file_id`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WeftFilesResponse {
    #[serde(default)]
    pub items: Vec<WeftFileRecord>,
    /// True when more pages follow. When the exact-path match is absent and
    /// `has_more` is true, the result is indeterminate — the file may be on a
    /// later page — so callers must degrade to `unavailable` rather than
    /// concluding `no_matches`.
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WeftObservationsResponse {
    #[serde(default)]
    pub items: Vec<ObservationRecord>,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub has_more: bool,
}

pub fn parse_wardline_findings_response(
    body: &str,
) -> Result<WardlineFindingsResponse, FiligreeContractError> {
    serde_json::from_str(body).map_err(FiligreeContractError::from)
}

pub fn parse_weft_files_response(body: &str) -> Result<WeftFilesResponse, FiligreeContractError> {
    serde_json::from_str(body).map_err(FiligreeContractError::from)
}

#[derive(Debug, Error)]
pub enum FiligreeContractError {
    #[error("invalid Filigree response: {0}")]
    InvalidResponse(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum FiligreeClientError {
    #[error("build Filigree HTTP client: {0}")]
    Build(#[source] reqwest::Error),

    #[error("request Filigree entity associations: {0}")]
    Request(#[source] reqwest::Error),

    #[error("Filigree returned HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },

    #[error("POST Filigree scan-results: {0}")]
    ScanResultsRequest(#[source] reqwest::Error),

    #[error("invalid Filigree scan-results response: {0}")]
    InvalidScanResultsResponse(#[source] serde_json::Error),

    #[error("POST Filigree clean-stale: {0}")]
    CleanStaleRequest(#[source] reqwest::Error),

    #[error("invalid Filigree clean-stale response: {0}")]
    InvalidCleanStaleResponse(#[source] serde_json::Error),

    #[error("request Filigree observations: {0}")]
    ObservationRequest(#[source] reqwest::Error),

    #[error("invalid Filigree observation response: {0}")]
    InvalidObservationResponse(#[source] serde_json::Error),

    #[error("run Filigree MCP tool {tool}: {message}")]
    McpTool { tool: String, message: String },

    #[error(transparent)]
    Contract(#[from] FiligreeContractError),
}

pub trait FiligreeLookup: Send + Sync {
    fn associations_for(
        &self,
        entity_id: &str,
    ) -> Result<EntityAssociationsResponse, FiligreeClientError>;

    /// Fetch an issue's title/status/priority to enrich an association match.
    /// Returns `Ok(None)` when the issue (or the detail route itself) is
    /// unavailable — a `404` — so callers degrade to issue-id-only rather than
    /// failing the whole `issues_for` call, per the enrich-only federation
    /// axiom. The default reports the route as unavailable; the HTTP client
    /// overrides it. A transport / non-404 HTTP failure is surfaced as `Err`
    /// so the caller can stop hammering a down endpoint.
    fn issue_detail(&self, _issue_id: &str) -> Result<Option<IssueDetail>, FiligreeClientError> {
        Ok(None)
    }

    /// Wardline findings for a source file, for read-time reconciliation
    /// (Flow B). Two-hop: resolve `path` -> Filigree `file_id`, then fetch that
    /// file's `scan_source=wardline` findings. Returns an empty list when no
    /// Wardline-touched file exists at `path`. Default impl returns empty (no
    /// Filigree); the HTTP client overrides it. Transport / non-success HTTP is
    /// surfaced as `Err` so the caller degrades the section to `unavailable`.
    fn wardline_findings_for_path(
        &self,
        _path: &str,
    ) -> Result<Vec<WardlineFinding>, FiligreeClientError> {
        Ok(Vec::new())
    }

    /// Create a pending Filigree observation. Default degrades to unavailable so
    /// tests/fake clients opt in explicitly and read-only deployments cannot
    /// accidentally pretend a proposal was recorded.
    fn create_observation(
        &self,
        _request: ObservationCreateRequest,
    ) -> Result<ObservationCreateResponse, FiligreeClientError> {
        Err(FiligreeClientError::McpTool {
            tool: "observation_create".to_owned(),
            message: "Filigree observation creation is unavailable".to_owned(),
        })
    }

    /// Fetch one pending observation by id. Default says "not found".
    fn observation_by_id(
        &self,
        _observation_id: &str,
    ) -> Result<Option<ObservationRecord>, FiligreeClientError> {
        Ok(None)
    }

    /// Mark a pending observation as consumed after Loomweave writes the local
    /// guidance sheet. Default no-ops so promotion remains local-first if the
    /// scratchpad cleanup route is unavailable.
    fn dismiss_observation(
        &self,
        _observation_id: &str,
        _reason: &str,
    ) -> Result<(), FiligreeClientError> {
        Ok(())
    }
}

/// Read the per-project federation token the Filigree daemon auto-mints at
/// `<root>/.weft/filigree/federation_token` on first serve (its inbound
/// resolver's tier 2; mirrored by wardline's credential loader). The file is
/// loopback deconfliction plumbing, not a secret — absence or unreadability
/// just means the rung resolves to None and auth stays off.
fn read_minted_federation_token(root: &Path) -> Option<String> {
    let path = root.join(".weft").join("filigree").join("federation_token");
    let raw = std::fs::read_to_string(path).ok()?;
    let token = raw.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_owned())
    }
}

#[derive(Debug, Clone)]
pub struct FiligreeHttpClient {
    base_url: String,
    /// Optional Filigree project key pinned on the scan-results emit (see
    /// [`FiligreeConfig::project`]). `None` → the unscoped single-project URL.
    project: Option<String>,
    actor: String,
    token: Option<String>,
    client: reqwest::blocking::Client,
    project_root: Option<PathBuf>,
}

impl FiligreeHttpClient {
    pub fn from_config<F>(
        config: &FiligreeConfig,
        env_lookup: F,
    ) -> Result<Option<Self>, FiligreeClientError>
    where
        F: Fn(&str) -> Option<String>,
    {
        Self::from_config_with_project_root(config, env_lookup, None)
    }

    pub fn from_config_with_project_root<F>(
        config: &FiligreeConfig,
        env_lookup: F,
        project_root: Option<&Path>,
    ) -> Result<Option<Self>, FiligreeClientError>
    where
        F: Fn(&str) -> Option<String>,
    {
        if !config.enabled {
            return Ok(None);
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(config.timeout_seconds.max(1)))
            .build()
            .map_err(FiligreeClientError::Build)?;
        // Resolve the configured env var (default `WEFT_FEDERATION_TOKEN`) first;
        // fall back to the legacy `FILIGREE_API_TOKEN` name so a pre-rename global
        // export keeps working during the transition (deprecated — remove once
        // operators have migrated to the Weft-prefixed name); finally fall back to
        // the token file the Filigree daemon auto-mints in the project store. That
        // last rung is the same-host zero-ceremony default (C-9e): the MCP server
        // is typically launched with an empty env, so without it every weft-gated
        // read (the wardline-findings joins) 401s — dogfood-4 A5.
        let token = env_lookup(&config.token_env)
            .filter(|value| !value.trim().is_empty())
            .or_else(|| env_lookup("FILIGREE_API_TOKEN").filter(|value| !value.trim().is_empty()))
            .or_else(|| project_root.and_then(read_minted_federation_token));
        Ok(Some(Self {
            base_url: config.base_url.clone(),
            project: config.project.clone(),
            actor: config.actor.clone(),
            token,
            client,
            project_root: project_root.map(Path::to_path_buf),
        }))
    }

    /// POST a scan-results batch to Filigree's native intake (WP9-B,
    /// REQ-FINDING-03). One-way Loomweave→Filigree push; the caller is expected to
    /// inspect [`ScanResultsResponse::warnings`] (severity coercion, unknown
    /// `scan_run_id`, etc.) rather than just the counts.
    ///
    /// # Errors
    ///
    /// Returns [`FiligreeClientError::ScanResultsRequest`] on transport failure,
    /// [`FiligreeClientError::HttpStatus`] on a non-success response (e.g. a
    /// `400 VALIDATION` for a malformed batch), or
    /// [`FiligreeClientError::InvalidScanResultsResponse`] when the body is not
    /// the expected shape.
    pub fn post_scan_results(
        &self,
        request: &ScanResultsRequest,
    ) -> Result<ScanResultsResponse, FiligreeClientError> {
        let mut http_request = self
            .client
            .post(scan_results_url(&self.base_url, self.project.as_deref()))
            .header("accept", "application/json")
            .json(request);
        if !self.actor.trim().is_empty() {
            http_request = http_request.header("x-filigree-actor", self.actor.as_str());
        }
        if let Some(token) = &self.token {
            http_request = http_request.bearer_auth(token);
        }
        let response = http_request
            .send()
            .map_err(FiligreeClientError::ScanResultsRequest)?;
        let status = response.status();
        let body = response
            .text()
            .map_err(FiligreeClientError::ScanResultsRequest)?;
        if !status.is_success() {
            return Err(FiligreeClientError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }
        parse_scan_results_response(&body).map_err(FiligreeClientError::InvalidScanResultsResponse)
    }

    /// POST a retention sweep to Filigree's `clean-stale` route (REQ-FINDING-06,
    /// `--prune-unseen`). One-way Loomweave→Filigree call; Filigree soft-archives
    /// its own `unseen_in_latest` findings for the given `scan_source`. The
    /// `scan_source` scoping is enforced server-side, so this can only sweep
    /// Loomweave's findings.
    ///
    /// # Errors
    ///
    /// Returns [`FiligreeClientError::CleanStaleRequest`] on transport failure,
    /// [`FiligreeClientError::HttpStatus`] on a non-success response, or
    /// [`FiligreeClientError::InvalidCleanStaleResponse`] when the body is not
    /// the expected shape.
    pub fn post_clean_stale(
        &self,
        request: &CleanStaleRequest,
    ) -> Result<CleanStaleResponse, FiligreeClientError> {
        let mut http_request = self
            .client
            .post(clean_stale_url(&self.base_url))
            .header("accept", "application/json")
            .json(request);
        if !self.actor.trim().is_empty() {
            http_request = http_request.header("x-filigree-actor", self.actor.as_str());
        }
        if let Some(token) = &self.token {
            http_request = http_request.bearer_auth(token);
        }
        let response = http_request
            .send()
            .map_err(FiligreeClientError::CleanStaleRequest)?;
        let status = response.status();
        let body = response
            .text()
            .map_err(FiligreeClientError::CleanStaleRequest)?;
        if !status.is_success() {
            return Err(FiligreeClientError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }
        parse_clean_stale_response(&body).map_err(FiligreeClientError::InvalidCleanStaleResponse)
    }

    /// GET `url` with the standard actor + bearer headers, returning the raw
    /// (unread) response. Shared by [`get_json`](Self::get_json) and
    /// [`get_json_or_none`](Self::get_json_or_none); the latter inspects the
    /// status before reading the body so a `404` can short-circuit.
    fn send_get(&self, url: &str) -> Result<reqwest::blocking::Response, FiligreeClientError> {
        let mut request = self.client.get(url).header("accept", "application/json");
        if !self.actor.trim().is_empty() {
            request = request.header("x-filigree-actor", self.actor.as_str());
        }
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        request.send().map_err(FiligreeClientError::Request)
    }

    /// GET `url` with the standard actor + bearer headers and parse the body as
    /// `T`. A non-success status is surfaced as `HttpStatus` so the caller can
    /// stop hammering a down endpoint.
    fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, FiligreeClientError> {
        let response = self.send_get(url)?;
        let status = response.status();
        let body = response.text().map_err(FiligreeClientError::Request)?;
        if !status.is_success() {
            return Err(FiligreeClientError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }
        serde_json::from_str(&body)
            .map_err(|e| FiligreeClientError::Contract(FiligreeContractError::from(e)))
    }

    /// Like [`get_json`](Self::get_json) but maps a `404` to `Ok(None)` — the
    /// enrich-only degrade signal for "the resource (or the route itself) is
    /// absent", not an error. The body is not read on a `404`. Any other
    /// non-success status is still surfaced as `HttpStatus`.
    fn get_json_or_none<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<Option<T>, FiligreeClientError> {
        let response = self.send_get(url)?;
        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let body = response.text().map_err(FiligreeClientError::Request)?;
        if !status.is_success() {
            return Err(FiligreeClientError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }
        serde_json::from_str(&body)
            .map(Some)
            .map_err(|e| FiligreeClientError::Contract(FiligreeContractError::from(e)))
    }

    fn run_mcp_tool(
        &self,
        tool: &str,
        arguments: &serde_json::Value,
    ) -> Result<serde_json::Value, FiligreeClientError> {
        let (program, args) = resolve_filigree_mcp_command(self.project_root.as_deref());
        run_mcp_tool_over_command(
            &program,
            &args,
            self.project_root.as_deref(),
            FILIGREE_MCP_TIMEOUT,
            tool,
            arguments,
        )
    }
}

/// Per-call timeout for the filigree MCP subprocess round-trip. filigree-mcp is
/// launched as a subprocess and driven over newline-delimited MCP JSON-RPC; a
/// child that accepts the connection and never answers would otherwise block the
/// observation write. 10s covers a cold filigree-mcp (Python import + DB open);
/// past it the call errors so the caller degrades instead of hanging.
const FILIGREE_MCP_TIMEOUT: Duration = Duration::from_secs(10);

/// Drive one filigree MCP tool call over the **newline-delimited** JSON-RPC
/// transport `filigree-mcp` (the MCP Python SDK's `stdio_server`) speaks — NOT
/// Content-Length framing, which it rejects as an internal error. The whole
/// handshake+call runs on a worker thread bounded by `timeout`: a hung child is
/// killed and the call errors rather than blocking forever. Returns the parsed
/// tool envelope (`result.content[0].text`). The resolved launch command is a
/// parameter so this is unit-testable with an injected fake server.
fn run_mcp_tool_over_command(
    program: &str,
    args: &[String],
    project_root: Option<&Path>,
    timeout: Duration,
    tool: &str,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, FiligreeClientError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // stderr discarded: we never drain it, so a large filigree traceback that
        // filled a piped stderr (64 KiB) would block the child mid-write.
        .stderr(Stdio::null())
        .current_dir(project_root.unwrap_or_else(|| Path::new(".")))
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

    let init = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "loomweave-init",
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "loomweave", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    let initialized = serde_json::json!({
        "jsonrpc": "2.0", "method": "notifications/initialized", "params": {}
    });
    let call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "loomweave-call",
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    });

    // Drive the blocking write+read on a worker thread so it is bounded by
    // recv_timeout; on timeout we kill the child (closing its pipes, unblocking
    // the worker). The handshake responses are tiny, so filigree never blocks on
    // stdout-write while we are still writing stdin — no write/read deadlock.
    let (tx, rx) = std::sync::mpsc::channel();
    let tool_owned = tool.to_owned();
    let worker = std::thread::spawn(move || {
        let outcome = (|| -> Result<serde_json::Value, FiligreeClientError> {
            write_mcp_json(&mut stdin, &init, &tool_owned)?;
            write_mcp_json(&mut stdin, &initialized, &tool_owned)?;
            write_mcp_json(&mut stdin, &call, &tool_owned)?;
            stdin
                .flush()
                .map_err(|err| mcp_tool_error(&tool_owned, &format!("flush stdin: {err}")))?;
            drop(stdin); // EOF ends filigree-mcp's read loop so it exits cleanly.
            let mut reader = BufReader::new(stdout);
            read_mcp_json(&mut reader, "loomweave-call", &tool_owned)
        })();
        let _ = tx.send(outcome);
    });

    let response = match rx.recv_timeout(timeout) {
        Ok(outcome) => {
            let _ = child.wait();
            let _ = worker.join();
            outcome?
        }
        Err(RecvTimeoutError::Timeout) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = worker.join();
            return Err(mcp_tool_error(
                tool,
                &format!("filigree did not respond within {}s", timeout.as_secs()),
            ));
        }
        Err(RecvTimeoutError::Disconnected) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(mcp_tool_error(
                tool,
                "filigree worker thread disconnected before responding",
            ));
        }
    };

    if let Some(error) = response.get("error").filter(|err| !err.is_null()) {
        return Err(mcp_tool_error(tool, &error.to_string()));
    }
    let text = response
        .get("result")
        .and_then(|result| result.get("content"))
        .and_then(serde_json::Value::as_array)
        .and_then(|content| content.first())
        .and_then(|item| item.get("text"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            mcp_tool_error(
                tool,
                &format!("missing result.content[0].text in response {response}"),
            )
        })?;
    let parsed: serde_json::Value =
        serde_json::from_str(text).map_err(FiligreeClientError::InvalidObservationResponse)?;
    if parsed.get("error").is_some() {
        return Err(mcp_tool_error(tool, &parsed.to_string()));
    }
    Ok(parsed)
}

/// Build a transport-level [`FiligreeClientError::McpTool`].
fn mcp_tool_error(tool: &str, message: &str) -> FiligreeClientError {
    FiligreeClientError::McpTool {
        tool: tool.to_owned(),
        message: message.to_owned(),
    }
}

impl FiligreeLookup for FiligreeHttpClient {
    fn associations_for(
        &self,
        entity_id: &str,
    ) -> Result<EntityAssociationsResponse, FiligreeClientError> {
        self.get_json(&entity_associations_url(&self.base_url, entity_id))
    }

    fn issue_detail(&self, issue_id: &str) -> Result<Option<IssueDetail>, FiligreeClientError> {
        // A 404 means the issue (or the whole detail route) is absent — the
        // enrich-only degrade signal, not an error — so use the `_or_none` form.
        self.get_json_or_none(&issue_detail_url(&self.base_url, issue_id))
    }

    fn wardline_findings_for_path(
        &self,
        path: &str,
    ) -> Result<Vec<WardlineFinding>, FiligreeClientError> {
        // Hop 1: path -> Filigree file_id. path_prefix is a prefix filter, so
        // take only the row whose path is byte-exact.
        let files: WeftFilesResponse =
            self.get_json(&weft_files_url(&self.base_url, "wardline", path))?;
        let exact = files.items.into_iter().find(|f| f.path == path);
        let Some(file_id) = exact.map(|f| f.file_id) else {
            // No exact match on this page. If has_more is true the result is
            // indeterminate — the file may be on a later page — so degrade to
            // unavailable rather than falsely concluding no_matches.
            if files.has_more {
                return Err(FiligreeClientError::HttpStatus {
                    status: 0,
                    body:
                        "weft/files truncated before exact path match; cannot conclude no findings"
                            .to_owned(),
                });
            }
            return Ok(Vec::new());
        };
        // Hop 2: file_id -> wardline findings. As with hop-1, Loomweave reads only
        // the first page; if it is truncated (`has_more`) the findings view is
        // incomplete, so fail closed to `unavailable` rather than returning a
        // silent undercount.
        let findings: WardlineFindingsResponse =
            self.get_json(&weft_findings_url(&self.base_url, "wardline", &file_id))?;
        if findings.has_more {
            return Err(FiligreeClientError::HttpStatus {
                status: 0,
                body: "weft/findings truncated; cannot enumerate all findings for file".to_owned(),
            });
        }
        Ok(findings.items)
    }

    fn create_observation(
        &self,
        request: ObservationCreateRequest,
    ) -> Result<ObservationCreateResponse, FiligreeClientError> {
        let mut arguments = serde_json::json!({
            "summary": request.summary,
            "detail": request.detail,
            "priority": request.priority,
            "actor": request.actor,
        });
        if let Some(obj) = arguments.as_object_mut() {
            if let Some(file_path) = request.file_path {
                obj.insert("file_path".to_owned(), serde_json::json!(file_path));
            }
            if let Some(line) = request.line {
                obj.insert("line".to_owned(), serde_json::json!(line));
            }
        }
        let value = self.run_mcp_tool("observation_create", &arguments)?;
        serde_json::from_value(value).map_err(FiligreeClientError::InvalidObservationResponse)
    }

    fn observation_by_id(
        &self,
        observation_id: &str,
    ) -> Result<Option<ObservationRecord>, FiligreeClientError> {
        let mut offset = 0_u64;
        let limit = 100_u64;
        loop {
            let page: WeftObservationsResponse =
                self.get_json(&weft_observations_url(&self.base_url, limit, offset))?;
            if let Some(found) = page
                .items
                .into_iter()
                .find(|item| item.observation_id == observation_id)
            {
                return Ok(Some(found));
            }
            if !page.has_more {
                return Ok(None);
            }
            offset = offset.saturating_add(limit);
        }
    }

    fn dismiss_observation(
        &self,
        observation_id: &str,
        reason: &str,
    ) -> Result<(), FiligreeClientError> {
        let arguments = serde_json::json!({
            "observation_id": observation_id,
            "reason": reason,
            "actor": self.actor.clone(),
        });
        let _ = self.run_mcp_tool("observation_dismiss", &arguments)?;
        Ok(())
    }
}

pub fn parse_entity_associations_response(
    body: &str,
) -> Result<EntityAssociationsResponse, FiligreeContractError> {
    serde_json::from_str(body).map_err(FiligreeContractError::from)
}

pub fn parse_issue_detail_response(body: &str) -> Result<IssueDetail, FiligreeContractError> {
    serde_json::from_str(body).map_err(FiligreeContractError::from)
}

pub fn issue_detail_url(base_url: &str, issue_id: &str) -> String {
    format!(
        "{}/api/weft/issues/{}",
        base_url.trim_end_matches('/'),
        percent_encode_query_value(issue_id)
    )
}

pub fn entity_associations_url(base_url: &str, entity_id: &str) -> String {
    format!(
        "{}/api/entity-associations?entity_id={}",
        base_url.trim_end_matches('/'),
        percent_encode_query_value(entity_id)
    )
}

pub fn weft_files_url(base_url: &str, scan_source: &str, path_prefix: &str) -> String {
    format!(
        "{}/api/weft/files?scan_source={}&path_prefix={}",
        base_url.trim_end_matches('/'),
        percent_encode_query_value(scan_source),
        percent_encode_query_value(path_prefix)
    )
}

pub fn weft_findings_url(base_url: &str, scan_source: &str, file_id: &str) -> String {
    format!(
        "{}/api/weft/findings?scan_source={}&file_id={}",
        base_url.trim_end_matches('/'),
        percent_encode_query_value(scan_source),
        percent_encode_query_value(file_id)
    )
}

pub fn weft_observations_url(base_url: &str, limit: u64, offset: u64) -> String {
    format!(
        "{}/api/weft/observations?limit={}&offset={}",
        base_url.trim_end_matches('/'),
        limit,
        offset
    )
}

/// Write one newline-delimited JSON-RPC message: compact JSON (no embedded
/// newlines) + `\n`, the framing `filigree-mcp` reads line-by-line.
fn write_mcp_json(
    writer: &mut impl Write,
    value: &serde_json::Value,
    tool: &str,
) -> Result<(), FiligreeClientError> {
    let mut body = serde_json::to_vec(value)
        .map_err(|err| mcp_tool_error(tool, &format!("serialize MCP request: {err}")))?;
    body.push(b'\n');
    writer
        .write_all(&body)
        .map_err(|err| mcp_tool_error(tool, &format!("write MCP request: {err}")))
}

/// Read newline-delimited JSON-RPC responses until one carries `expected_id`,
/// skipping the init result and the notification's `id: null` error. EOF before
/// a match is a transport fault (surfaced so the caller degrades).
fn read_mcp_json(
    reader: &mut impl BufRead,
    expected_id: &str,
    tool: &str,
) -> Result<serde_json::Value, FiligreeClientError> {
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader
            .read_line(&mut line)
            .map_err(|err| mcp_tool_error(tool, &format!("read MCP response: {err}")))?;
        if read == 0 {
            return Err(mcp_tool_error(
                tool,
                "filigree closed its output before answering the call",
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
        // Non-matching id (init result, notification id:null error) → keep reading.
    }
}

fn resolve_filigree_mcp_command(project_root: Option<&Path>) -> (String, Vec<String>) {
    if let Ok(raw) = std::env::var("LOOMWEAVE_FILIGREE_MCP_COMMAND") {
        let mut parts: Vec<String> = raw
            .split_whitespace()
            .map(|part| replace_project_placeholder(part, project_root))
            .collect();
        if let Some(program) = parts.first().cloned() {
            parts.remove(0);
            return (program, parts);
        }
    }

    let mut status_cmd = Command::new("filigree");
    status_cmd.args(["mcp-status", "--json"]);
    if let Some(root) = project_root {
        status_cmd.current_dir(root);
    }
    if let Ok(output) = status_cmd.output()
        && output.status.success()
        && let Ok(status) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        && let Some(python) = status
            .get("runtime")
            .and_then(|runtime| runtime.get("python_executable"))
            .and_then(serde_json::Value::as_str)
    {
        let mut args = vec!["-m".to_owned(), "filigree.mcp_server".to_owned()];
        if let Some(root) = project_root {
            args.push("--project".to_owned());
            args.push(root.display().to_string());
        }
        return (python.to_owned(), args);
    }

    filigree_mcp_fallback_command()
}

/// Last-resort launcher when the env override is unset and `filigree mcp-status`
/// could not name a python executable: the standalone `filigree-mcp` binary.
/// NOT `filigree mcp` — that is not a valid filigree subcommand (it exits with a
/// usage error → broken pipe), the defect the Warpline consumer hit.
fn filigree_mcp_fallback_command() -> (String, Vec<String>) {
    ("filigree-mcp".to_owned(), Vec::new())
}

fn replace_project_placeholder(raw: &str, project_root: Option<&Path>) -> String {
    match project_root {
        Some(root) => raw.replace("{project}", &root.display().to_string()),
        None => raw.to_owned(),
    }
}

fn percent_encode_query_value(raw: &str) -> String {
    let mut encoded = String::new();
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => {
                encoded.push('%');
                encoded.push(hex_digit(byte >> 4));
                encoded.push(hex_digit(byte & 0x0f));
            }
        }
    }
    encoded
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => char::from(b'0' + value),
        10..=15 => char::from(b'A' + (value - 10)),
        _ => unreachable!("nibble is always <= 15"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn canonical_entity_association_fixture_body(example_name: &str) -> serde_json::Value {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/federation/fixtures/filigree-entity-associations-response.json"
        ))
        .expect("parse canonical Filigree EntityAssociation fixture");
        fixture
            .get("examples")
            .and_then(serde_json::Value::as_array)
            .and_then(|examples| {
                examples.iter().find(|example| {
                    example.get("name").and_then(serde_json::Value::as_str) == Some(example_name)
                })
            })
            .and_then(|example| example.pointer("/response/body"))
            .cloned()
            .unwrap_or_else(|| panic!("missing fixture example body {example_name}"))
    }

    /// Minimal enabled config; `from_config` does not connect until a request is
    /// issued, so no server is needed to exercise token resolution.
    fn token_resolution_config() -> FiligreeConfig {
        FiligreeConfig {
            enabled: true,
            base_url: "http://127.0.0.1:1".to_owned(),
            project: None,
            actor: "loomweave-test".to_owned(),
            token_env: "WEFT_FEDERATION_TOKEN".to_owned(),
            timeout_seconds: 1,
            emit_findings: false,
            prune_unseen_days: 30,
        }
    }

    fn resolved_token(env: &[(&str, &str)]) -> Option<String> {
        let config = token_resolution_config();
        FiligreeHttpClient::from_config(&config, |name| {
            env.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
        .expect("build client")
        .expect("enabled client")
        .token
    }

    #[test]
    fn token_resolution_prefers_configured_env_var() {
        assert_eq!(
            resolved_token(&[("WEFT_FEDERATION_TOKEN", "new-secret")]),
            Some("new-secret".to_owned()),
        );
    }

    #[test]
    fn token_resolution_falls_back_to_legacy_filigree_api_token() {
        // Pre-rename global export still works during the transition.
        assert_eq!(
            resolved_token(&[("FILIGREE_API_TOKEN", "legacy-secret")]),
            Some("legacy-secret".to_owned()),
        );
    }

    #[test]
    fn token_resolution_configured_var_wins_over_legacy_fallback() {
        assert_eq!(
            resolved_token(&[
                ("WEFT_FEDERATION_TOKEN", "new-secret"),
                ("FILIGREE_API_TOKEN", "legacy-secret"),
            ]),
            Some("new-secret".to_owned()),
        );
    }

    #[test]
    fn token_resolution_empty_configured_var_falls_through_to_legacy() {
        assert_eq!(
            resolved_token(&[
                ("WEFT_FEDERATION_TOKEN", "   "),
                ("FILIGREE_API_TOKEN", "legacy-secret"),
            ]),
            Some("legacy-secret".to_owned()),
        );
    }

    #[test]
    fn token_resolution_none_when_neither_set() {
        assert_eq!(resolved_token(&[]), None);
    }

    fn mint_token_file(root: &Path, contents: &str) {
        let dir = root.join(".weft").join("filigree");
        std::fs::create_dir_all(&dir).expect("create store dir");
        std::fs::write(dir.join("federation_token"), contents).expect("write token");
    }

    fn resolved_token_with_root(env: &[(&str, &str)], root: &Path) -> Option<String> {
        let config = token_resolution_config();
        FiligreeHttpClient::from_config_with_project_root(
            &config,
            |name| {
                env.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            },
            Some(root),
        )
        .expect("build client")
        .expect("enabled client")
        .token
    }

    #[test]
    fn token_resolution_falls_back_to_minted_project_store_file() {
        // Dogfood-4 A5: the MCP serve path launches with an empty env; the
        // daemon's auto-minted token file is the same-host zero-ceremony rung.
        let dir = tempfile::tempdir().expect("tempdir");
        mint_token_file(dir.path(), "minted-secret\n");
        assert_eq!(
            resolved_token_with_root(&[], dir.path()),
            Some("minted-secret".to_owned()),
        );
    }

    #[test]
    fn token_resolution_env_wins_over_minted_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        mint_token_file(dir.path(), "minted-secret");
        assert_eq!(
            resolved_token_with_root(&[("WEFT_FEDERATION_TOKEN", "env-secret")], dir.path()),
            Some("env-secret".to_owned()),
        );
    }

    #[test]
    fn token_resolution_none_when_minted_file_absent_or_blank() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(resolved_token_with_root(&[], dir.path()), None);
        mint_token_file(dir.path(), "   \n");
        assert_eq!(resolved_token_with_root(&[], dir.path()), None);
    }

    #[test]
    fn parses_reverse_entity_association_response_shape() {
        let parsed = parse_entity_associations_response(
            r#"{
                "associations": [
                    {
                        "issue_id": "filigree-1234567890",
                        "loomweave_entity_id": "python:function:demo.hello",
                        "content_hash_at_attach": "hash-a",
                        "attached_at": "2026-05-17T00:00:00.000Z",
                        "attached_by": "codex"
                    }
                ]
            }"#,
        )
        .expect("parse Filigree reverse route response");

        assert_eq!(parsed.associations.len(), 1);
        let row = &parsed.associations[0];
        assert_eq!(row.issue_id, "filigree-1234567890");
        assert_eq!(row.loomweave_entity_id, "python:function:demo.hello");
        assert_eq!(row.content_hash_at_attach, "hash-a");
        assert_eq!(row.attached_at, "2026-05-17T00:00:00.000Z");
        assert_eq!(row.attached_by, "codex");
    }

    #[test]
    fn parses_reverse_entity_association_response_with_sei_key() {
        let parsed = parse_entity_associations_response(
            r#"{
                "associations": [
                    {
                        "issue_id": "filigree-1234567890",
                        "loomweave_entity_id": "loomweave:eid:0123456789abcdef0123456789abcdef",
                        "content_hash_at_attach": "hash-a",
                        "attached_at": "2026-05-17T00:00:00.000Z",
                        "attached_by": "codex"
                    }
                ]
            }"#,
        )
        .expect("parse SEI-keyed Filigree reverse route response");

        assert_eq!(
            parsed.associations[0].loomweave_entity_id,
            "loomweave:eid:0123456789abcdef0123456789abcdef"
        );
    }

    // --- G15: rename/drift-tolerant deserialization (clarion-18d0f42964) ---
    // Defensive half only; the shared cross-member conformance vector is the
    // deferred producer-coupled half. See `residual` note at the end.

    /// The live Filigree v27 producer co-emits BOTH `entity_id` and
    /// `loomweave_entity_id` (identical values) plus governance/computed/null
    /// fields the consumer does not model. This MUST parse — and it is the
    /// regression guard for the alias trap: serde rejects a duplicate field
    /// slot, so if `loomweave_entity_id` ever gains `alias = "entity_id"` this
    /// vector fails with `duplicate field`. `alias = "clarion_entity_id"` is
    /// safe here because `clarion_entity_id` is absent.
    #[test]
    fn parses_live_v27_both_keys_and_governance_fields() {
        let parsed = parse_entity_associations_response(
            r#"{"associations":[{
                "issue_id":"filigree-1234567890",
                "entity_id":"loomweave:eid:0123456789abcdef0123456789abcdef",
                "loomweave_entity_id":"loomweave:eid:0123456789abcdef0123456789abcdef",
                "entity_kind":"function",
                "content_hash_at_attach":"hash-a",
                "attached_at":"2026-05-17T00:00:00.000Z",
                "attached_by":"codex",
                "migration_orphaned_at":null,
                "orphan_status":"unknown",
                "freshness_status":"unknown",
                "signature":null,"signoff_seq":null,"signed_content_hash":null
            }]}"#,
        )
        .expect("live v27 both-keys shape must parse (no duplicate-field trap)");
        assert_eq!(parsed.associations.len(), 1);
        assert_eq!(
            parsed.associations[0].loomweave_entity_id,
            "loomweave:eid:0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn parses_canonical_filigree_entity_association_fixture() {
        let body = canonical_entity_association_fixture_body("live_v27_reverse_lookup_200");
        let parsed = parse_entity_associations_response(&body.to_string())
            .expect("canonical live Filigree EntityAssociation fixture must deserialize");

        assert_eq!(parsed.associations.len(), 1);
        let row = &parsed.associations[0];
        assert_eq!(row.issue_id, "test-045076e30f");
        assert_eq!(
            row.loomweave_entity_id,
            "loomweave:eid:0123456789abcdef0123456789abcdef"
        );
        assert_eq!(row.content_hash_at_attach, "hash-g15-oracle");
        assert_eq!(row.attached_at, "2026-06-13T00:00:00+00:00");
        assert_eq!(row.attached_by, "g15-oracle");
    }

    /// A pre-v26 producer (or pre-v26 JSONL export) names the same value
    /// `clarion_entity_id`; the alias routes it into the canonical slot.
    #[test]
    fn parses_pre_v26_clarion_entity_id_via_alias() {
        let parsed = parse_entity_associations_response(
            r#"{"associations":[{
                "issue_id":"filigree-1234567890",
                "clarion_entity_id":"python:function:demo.hello",
                "content_hash_at_attach":"hash-a",
                "attached_at":"2026-05-17T00:00:00.000Z",
                "attached_by":"codex"
            }]}"#,
        )
        .expect("pre-v26 clarion_entity_id must deserialize via alias");
        assert_eq!(
            parsed.associations[0].loomweave_entity_id,
            "python:function:demo.hello"
        );
    }

    /// Unknown/future producer fields are ignored (no `deny_unknown_fields`).
    #[test]
    fn ignores_unknown_producer_fields() {
        let parsed = parse_entity_associations_response(
            r#"{"associations":[{
                "issue_id":"filigree-1",
                "loomweave_entity_id":"python:function:demo.hello",
                "content_hash_at_attach":"hash-a",
                "attached_at":"2026-05-17T00:00:00.000Z",
                "attached_by":"codex",
                "some_future_field":{"nested":true}
            }]}"#,
        )
        .expect("unknown fields must be ignored, not rejected");
        assert_eq!(
            parsed.associations[0].loomweave_entity_id,
            "python:function:demo.hello"
        );
    }

    /// Dropped display-only fields degrade to empty strings via `default`,
    /// rather than hard-failing the whole read.
    #[test]
    fn defaults_absent_display_fields() {
        let parsed = parse_entity_associations_response(
            r#"{"associations":[{
                "issue_id":"filigree-1",
                "loomweave_entity_id":"python:function:demo.hello",
                "content_hash_at_attach":"hash-a"
            }]}"#,
        )
        .expect("absent attached_at/attached_by must default, not fail");
        let row = &parsed.associations[0];
        assert_eq!(row.attached_at, "");
        assert_eq!(row.attached_by, "");
        assert_eq!(row.content_hash_at_attach, "hash-a");
    }

    /// An absent top-level `associations` key degrades to an empty list
    /// (enrich-only), rather than erroring `missing field associations`.
    #[test]
    fn defaults_absent_envelope_to_empty_list() {
        let parsed = parse_entity_associations_response("{}")
            .expect("absent envelope key must default to empty list");
        assert!(parsed.associations.is_empty());
    }

    #[test]
    fn builds_reverse_route_url_with_encoded_entity_id() {
        let url = entity_associations_url("http://127.0.0.1:8766/", "python:function:demo.hello");

        assert_eq!(
            url,
            "http://127.0.0.1:8766/api/entity-associations?entity_id=python%3Afunction%3Ademo.hello"
        );
    }

    #[test]
    fn builds_reverse_route_url_with_encoded_sei_key() {
        let url = entity_associations_url(
            "http://127.0.0.1:8766/",
            "loomweave:eid:0123456789abcdef0123456789abcdef",
        );

        assert_eq!(
            url,
            "http://127.0.0.1:8766/api/entity-associations?entity_id=loomweave%3Aeid%3A0123456789abcdef0123456789abcdef"
        );
    }

    #[test]
    fn http_client_hits_reverse_route_with_actor_and_bearer_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains(
                "GET /api/entity-associations?entity_id=python%3Afunction%3Ademo.hello HTTP/1.1"
            ));
            assert!(request.contains("x-filigree-actor: loomweave-test"));
            assert!(request.contains("authorization: Bearer secret-token"));

            let body = r#"{"associations":[{"issue_id":"filigree-1234567890","loomweave_entity_id":"python:function:demo.hello","content_hash_at_attach":"hash-a","attached_at":"2026-05-17T00:00:00.000Z","attached_by":"codex"}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let config = FiligreeConfig {
            enabled: true,
            base_url: format!("http://{addr}"),
            project: None,
            actor: "loomweave-test".to_owned(),
            token_env: "TEST_FILIGREE_TOKEN".to_owned(),
            timeout_seconds: 1,
            emit_findings: true,
            prune_unseen_days: 30,
        };
        let client = FiligreeHttpClient::from_config(&config, |name| {
            (name == "TEST_FILIGREE_TOKEN").then(|| "secret-token".to_owned())
        })
        .expect("build client")
        .expect("enabled client");

        let response = client
            .associations_for("python:function:demo.hello")
            .expect("fetch associations");

        assert_eq!(response.associations[0].issue_id, "filigree-1234567890");
        handle.join().expect("server thread");
    }

    #[test]
    fn parses_issue_detail_response_shape() {
        let parsed = parse_issue_detail_response(
            r#"{
                "issue_id": "clarion-51a2868c86",
                "title": "issues_for: enrich matches",
                "status": "proposed",
                "status_category": "open",
                "priority": 3,
                "type": "feature"
            }"#,
        )
        .expect("parse issue detail");
        assert_eq!(parsed.title, "issues_for: enrich matches");
        assert_eq!(parsed.status, "proposed");
        assert_eq!(parsed.priority, 3);
    }

    #[test]
    fn builds_issue_detail_url_with_encoded_id() {
        let url = issue_detail_url("http://127.0.0.1:8542/", "clarion-51a2868c86");
        assert_eq!(
            url,
            "http://127.0.0.1:8542/api/weft/issues/clarion-51a2868c86"
        );
    }

    #[test]
    fn issue_detail_http_client_parses_200() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.contains("GET /api/weft/issues/clarion-51a2868c86 HTTP/1.1"));

            let body = r#"{"issue_id":"clarion-51a2868c86","title":"enrich","status":"proposed","priority":3}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let client = detail_test_client(addr);
        let detail = client
            .issue_detail("clarion-51a2868c86")
            .expect("issue detail request")
            .expect("issue present");
        assert_eq!(
            detail.id, "clarion-51a2868c86",
            "id deserializes from the route's issue_id field (weft-4a46553503)"
        );
        assert_eq!(detail.title, "enrich");
        assert_eq!(detail.status, "proposed");
        assert_eq!(detail.priority, 3);
        handle.join().expect("server thread");
    }

    #[test]
    fn issue_detail_http_client_maps_404_to_none() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).expect("read request");
            let body = r#"{"error":"Not Found","code":"NOT_FOUND"}"#;
            write!(
                stream,
                "HTTP/1.1 404 Not Found\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let client = detail_test_client(addr);
        let detail = client
            .issue_detail("loomweave-missing")
            .expect("404 is Ok(None), not an error");
        assert!(detail.is_none(), "404 degrades to None: {detail:?}");
        handle.join().expect("server thread");
    }

    #[test]
    fn post_scan_results_sends_batch_and_parses_response() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 8192];
            let read = stream.read(&mut request).expect("read request");
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(
                request.contains("POST /api/v1/scan-results HTTP/1.1"),
                "request line: {request}"
            );
            assert!(request.contains("x-filigree-actor: loomweave-test"));
            assert!(request.contains("authorization: Bearer secret-token"));
            // The wire body carries the mapped severity, not the internal one.
            assert!(
                request.contains("\"scan_source\":\"loomweave\""),
                "body: {request}"
            );
            assert!(
                request.contains("\"severity\":\"medium\""),
                "body: {request}"
            );
            assert!(
                request.contains("\"internal_severity\":\"WARN\""),
                "body: {request}"
            );

            let body = r#"{"files_created":1,"files_updated":0,"findings_created":1,"findings_updated":0,"new_finding_ids":["clarion-sf-abc"],"observations_created":0,"observations_failed":0,"warnings":["Scan run run-1 status not updated to 'completed': not found"]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let config = FiligreeConfig {
            enabled: true,
            base_url: format!("http://{addr}"),
            project: None,
            actor: "loomweave-test".to_owned(),
            token_env: "TEST_FILIGREE_TOKEN".to_owned(),
            timeout_seconds: 1,
            emit_findings: true,
            prune_unseen_days: 30,
        };
        let client = FiligreeHttpClient::from_config(&config, |name| {
            (name == "TEST_FILIGREE_TOKEN").then(|| "secret-token".to_owned())
        })
        .expect("build client")
        .expect("enabled client");

        let row = crate::scan_results::FindingForEmit {
            id: "core:finding:circular".to_owned(),
            rule_id: "LMWV-PY-STRUCTURE-001".to_owned(),
            kind: "defect".to_owned(),
            severity: "WARN".to_owned(),
            confidence: Some(0.9),
            confidence_basis: None,
            message: "Circular import".to_owned(),
            entity_id: "python:class:auth.tokens::TokenManager".to_owned(),
            related_entities_json: "[]".to_owned(),
            supports_json: "[]".to_owned(),
            supported_by_json: "[]".to_owned(),
            source_file_path: Some("src/auth/tokens.py".to_owned()),
            source_line_start: Some(12),
            source_line_end: Some(12),
        };
        let batch = crate::scan_results::prepare_batch(
            &[row],
            &crate::scan_results::EmitOptions {
                scan_run_id: Some("run-1".to_owned()),
                mark_unseen: true,
                complete_scan_run: true,
                default_path: None,
            },
        );

        let response = client
            .post_scan_results(&batch.request)
            .expect("post scan results");
        assert_eq!(response.findings_created, 1);
        assert_eq!(response.new_finding_ids, vec!["clarion-sf-abc"]);
        assert_eq!(response.warnings.len(), 1);
        handle.join().expect("server thread");
    }

    #[test]
    fn post_scan_results_surfaces_validation_error_as_http_status() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut request = [0_u8; 8192];
            let _ = stream.read(&mut request).expect("read request");
            let body =
                r#"{"error":"findings[0] is missing required key 'path'","code":"VALIDATION"}"#;
            write!(
                stream,
                "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .expect("write response");
        });
        let client = detail_test_client(addr);
        let batch = crate::scan_results::prepare_batch(
            &[],
            &crate::scan_results::EmitOptions {
                scan_run_id: None,
                mark_unseen: true,
                complete_scan_run: true,
                default_path: None,
            },
        );
        let err = client
            .post_scan_results(&batch.request)
            .expect_err("400 surfaces as error");
        match err {
            FiligreeClientError::HttpStatus { status, .. } => assert_eq!(status, 400),
            other => panic!("expected HttpStatus, got {other:?}"),
        }
        handle.join().expect("server thread");
    }

    #[test]
    fn parses_weft_findings_list_envelope() {
        let resp = parse_wardline_findings_response(
            r#"{"items":[
                {"finding_id":"f-1","file_id":"file-9","severity":"high","status":"open",
                 "scan_source":"wardline","rule_id":"WLN-TAINT-001","message":"tainted sink",
                 "suggestion":"","scan_run_id":"r-1","line_start":12,"line_end":12,
                 "fingerprint":"fp-abc","issue_id":null,"seen_count":1,
                 "metadata":{"wardline":{"qualname":"demo.Foo.bar","kind":"DEFECT"}},
                 "data_warnings":[]}
            ],"has_more":false}"#,
        )
        .expect("parse findings list");
        assert_eq!(resp.items.len(), 1);
        let f = &resp.items[0];
        assert_eq!(f.rule_id, "WLN-TAINT-001");
        assert_eq!(f.fingerprint.as_deref(), Some("fp-abc"));
        assert_eq!(f.line_start, Some(12));
        assert_eq!(
            f.metadata
                .get("wardline")
                .and_then(|w| w.get("qualname"))
                .and_then(|q| q.as_str()),
            Some("demo.Foo.bar")
        );
    }

    #[test]
    fn parses_weft_files_list_envelope() {
        let resp = parse_weft_files_response(
            r#"{"items":[
                {"file_id":"file-9","path":"src/demo.py","language":"python","file_type":"source"},
                {"file_id":"file-10","path":"src/demo_helpers.py","language":"python","file_type":"source"}
            ],"has_more":false}"#,
        )
        .expect("parse files list");
        assert_eq!(resp.items.len(), 2);
        assert_eq!(resp.items[0].file_id, "file-9");
        assert_eq!(resp.items[0].path, "src/demo.py");
    }

    #[test]
    fn builds_weft_url_builders_with_encoding() {
        assert_eq!(
            weft_files_url("http://127.0.0.1:8542/", "wardline", "src/demo.py"),
            "http://127.0.0.1:8542/api/weft/files?scan_source=wardline&path_prefix=src%2Fdemo.py"
        );
        assert_eq!(
            weft_findings_url("http://127.0.0.1:8542/", "wardline", "file-9"),
            "http://127.0.0.1:8542/api/weft/findings?scan_source=wardline&file_id=file-9"
        );
    }

    #[test]
    fn wardline_findings_for_path_does_two_hops_and_exact_path_filter() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            // Hop 1: GET /api/weft/files — path_prefix matches two files; the
            // exact-path filter must pick file-9, not the helpers file.
            let (mut s1, _) = listener.accept().expect("accept files");
            let mut buf = [0_u8; 4096];
            let n = s1.read(&mut buf).expect("read files req");
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains(
                "GET /api/weft/files?scan_source=wardline&path_prefix=src%2Fdemo.py HTTP/1.1"
            ));
            let body = r#"{"items":[{"file_id":"file-9","path":"src/demo.py","language":"python","file_type":"source"},{"file_id":"file-10","path":"src/demo.py.bak","language":"python","file_type":"source"}],"has_more":false}"#;
            // connection: close forces reqwest to open a fresh TCP connection for
            // hop 2, so the listener's second accept() receives it (the blocking
            // client would otherwise pool/reuse hop-1's socket and hop-2's
            // accept() would hang).
            write!(
                s1,
                "HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();

            // Hop 2: GET /api/weft/findings for file-9.
            let (mut s2, _) = listener.accept().expect("accept findings");
            let n = s2.read(&mut buf).expect("read findings req");
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.contains("GET /api/weft/findings?scan_source=wardline&file_id=file-9 HTTP/1.1")
            );
            let body = r#"{"items":[{"finding_id":"f-1","file_id":"file-9","severity":"high","status":"open","scan_source":"wardline","rule_id":"WLN-TAINT-001","message":"sink","suggestion":"","scan_run_id":"r-1","line_start":12,"line_end":12,"fingerprint":"fp","issue_id":null,"seen_count":1,"metadata":{"wardline":{"qualname":"demo.Foo.bar"}},"data_warnings":[]}],"has_more":false}"#;
            write!(
                s2,
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        // Not detail_test_client(addr): the two-hop test does two sequential TCP
        // accepts, so use a more generous timeout to avoid CI scheduling jitter
        // between hops.
        let config = FiligreeConfig {
            enabled: true,
            base_url: format!("http://{addr}"),
            project: None,
            actor: "loomweave-test".to_owned(),
            token_env: "TEST_FILIGREE_TOKEN".to_owned(),
            timeout_seconds: 5,
            emit_findings: true,
            prune_unseen_days: 30,
        };
        let client = FiligreeHttpClient::from_config(&config, |_| None)
            .expect("build client")
            .expect("enabled client");
        let findings = client
            .wardline_findings_for_path("src/demo.py")
            .expect("two-hop fetch");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule_id, "WLN-TAINT-001");
        handle.join().expect("server thread");
    }

    /// FIX 3: when hop-1 returns items that don't include the exact path AND
    /// `has_more` is true, `wardline_findings_for_path` must return `Err` rather
    /// than `Ok(empty)` — a truncated page is indeterminate, not "no file found".
    #[test]
    fn wardline_findings_for_path_errors_when_hop1_truncated_before_exact_match() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            // Hop 1: page does NOT contain src/demo.py but has_more is true —
            // the exact path may be on a later page.
            let (mut s1, _) = listener.accept().expect("accept files");
            let mut buf = [0_u8; 4096];
            let n = s1.read(&mut buf).expect("read files req");
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains(
                "GET /api/weft/files?scan_source=wardline&path_prefix=src%2Fdemo.py HTTP/1.1"
            ));
            // Return a page that omits the target path with has_more:true.
            let body = r#"{"items":[{"file_id":"file-1","path":"src/demo_other.py","language":"python","file_type":"source"}],"has_more":true}"#;
            write!(
                s1,
                "HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            // No hop-2 — the function must error before making a second request.
        });
        let config = FiligreeConfig {
            enabled: true,
            base_url: format!("http://{addr}"),
            project: None,
            actor: "loomweave-test".to_owned(),
            token_env: "TEST_FILIGREE_TOKEN".to_owned(),
            timeout_seconds: 5,
            emit_findings: true,
            prune_unseen_days: 30,
        };
        let client = FiligreeHttpClient::from_config(&config, |_| None)
            .expect("build client")
            .expect("enabled client");
        let result = client.wardline_findings_for_path("src/demo.py");
        handle.join().expect("server thread");
        assert!(
            result.is_err(),
            "truncated hop-1 without exact match must be Err, not Ok: {result:?}"
        );
    }

    /// Hop-2 counterpart to the hop-1 truncation test: when the findings page
    /// for the resolved `file_id` reports `has_more: true`, the first page is an
    /// incomplete view, so `wardline_findings_for_path` must return `Err`
    /// (degrades to `unavailable`) rather than `Ok(partial)` — no silent
    /// undercount.
    #[test]
    fn wardline_findings_for_path_errors_when_hop2_truncated() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            // Hop 1: exact path resolves to file-9 on a complete page.
            let (mut s1, _) = listener.accept().expect("accept files");
            let mut buf = [0_u8; 4096];
            let n = s1.read(&mut buf).expect("read files req");
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains(
                "GET /api/weft/files?scan_source=wardline&path_prefix=src%2Fdemo.py HTTP/1.1"
            ));
            let body = r#"{"items":[{"file_id":"file-9","path":"src/demo.py","language":"python","file_type":"source"}],"has_more":false}"#;
            write!(
                s1,
                "HTTP/1.1 200 OK\r\nconnection: close\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();

            // Hop 2: findings page for file-9 is truncated (has_more:true).
            let (mut s2, _) = listener.accept().expect("accept findings");
            let n = s2.read(&mut buf).expect("read findings req");
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.contains("GET /api/weft/findings?scan_source=wardline&file_id=file-9 HTTP/1.1")
            );
            let body = r#"{"items":[{"finding_id":"f-1","file_id":"file-9","severity":"high","status":"open","scan_source":"wardline","rule_id":"WLN-TAINT-001","message":"sink","suggestion":"","scan_run_id":"r-1","line_start":12,"line_end":12,"fingerprint":"fp","issue_id":null,"seen_count":1,"metadata":{"wardline":{"qualname":"demo.Foo.bar"}},"data_warnings":[]}],"has_more":true}"#;
            write!(
                s2,
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let config = FiligreeConfig {
            enabled: true,
            base_url: format!("http://{addr}"),
            project: None,
            actor: "loomweave-test".to_owned(),
            token_env: "TEST_FILIGREE_TOKEN".to_owned(),
            timeout_seconds: 5,
            emit_findings: true,
            prune_unseen_days: 30,
        };
        let client = FiligreeHttpClient::from_config(&config, |_| None)
            .expect("build client")
            .expect("enabled client");
        let result = client.wardline_findings_for_path("src/demo.py");
        handle.join().expect("server thread");
        assert!(
            result.is_err(),
            "truncated hop-2 findings page must be Err, not Ok(partial): {result:?}"
        );
    }

    fn detail_test_client(addr: std::net::SocketAddr) -> FiligreeHttpClient {
        let config = FiligreeConfig {
            enabled: true,
            base_url: format!("http://{addr}"),
            project: None,
            actor: "loomweave-test".to_owned(),
            token_env: "TEST_FILIGREE_TOKEN".to_owned(),
            timeout_seconds: 1,
            emit_findings: true,
            prune_unseen_days: 30,
        };
        FiligreeHttpClient::from_config(&config, |_| None)
            .expect("build client")
            .expect("enabled client")
    }

    // --- MCP stdio transport (newline-delimited JSON-RPC) ---------------------

    /// `write_mcp_json` must emit ONE newline-delimited JSON line — the framing
    /// the MCP Python SDK's stdio transport (`mcp.server.stdio`) reads — NOT a
    /// Content-Length frame (which filigree-mcp rejects as an internal error).
    #[test]
    fn write_mcp_json_emits_newline_delimited_line_not_content_length() {
        let mut buf: Vec<u8> = Vec::new();
        write_mcp_json(
            &mut buf,
            &serde_json::json!({"jsonrpc": "2.0", "id": "x", "method": "ping"}),
            "ping",
        )
        .expect("write");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(
            !text.contains("Content-Length"),
            "must NOT use Content-Length framing: {text:?}"
        );
        assert!(text.ends_with('\n'), "must be newline-terminated: {text:?}");
        assert_eq!(text.matches('\n').count(), 1, "exactly one line: {text:?}");
        let parsed: serde_json::Value =
            serde_json::from_str(text.trim_end()).expect("the line is the JSON body");
        assert_eq!(parsed["id"], serde_json::json!("x"));
    }

    /// `read_mcp_json` must read newline-delimited responses, skipping lines whose
    /// id does not match (the init result, the notification's id:null error) until
    /// the awaited id.
    #[test]
    fn read_mcp_json_reads_newline_lines_and_skips_non_matching_ids() {
        let stream = "\
{\"jsonrpc\":\"2.0\",\"id\":\"loomweave-init\",\"result\":{}}\n\
{\"jsonrpc\":\"2.0\",\"id\":null,\"error\":{\"code\":-32601}}\n\
{\"jsonrpc\":\"2.0\",\"id\":\"loomweave-call\",\"result\":{\"ok\":true}}\n";
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(stream.as_bytes()));
        let value = read_mcp_json(&mut reader, "loomweave-call", "observation_create")
            .expect("reads the matching-id line");
        assert_eq!(value["result"]["ok"], serde_json::json!(true));
    }

    /// EOF before the awaited id is a transport fault, surfaced as an error (never
    /// a silent success) so the caller degrades.
    #[test]
    fn read_mcp_json_errors_on_eof_before_match() {
        let stream = "{\"jsonrpc\":\"2.0\",\"id\":\"loomweave-init\",\"result\":{}}\n";
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(stream.as_bytes()));
        let err = read_mcp_json(&mut reader, "loomweave-call", "observation_create")
            .expect_err("EOF before the call response must error");
        assert!(matches!(err, FiligreeClientError::McpTool { .. }), "{err}");
    }

    /// The last-resort launcher fallback is the standalone `filigree-mcp` binary,
    /// NOT `filigree mcp` (which is not a valid filigree subcommand and would exit
    /// with a usage error → broken pipe, the same defect the Warpline consumer
    /// hit). The happy path resolves `python -m filigree.mcp_server` via
    /// `filigree mcp-status`; this guards only the fallback constant.
    #[test]
    fn fallback_command_is_filigree_mcp_binary_not_subcommand() {
        let (program, args) = filigree_mcp_fallback_command();
        assert_eq!(program, "filigree-mcp");
        assert!(
            args.is_empty(),
            "the MCP server takes no subcommand: {args:?}"
        );
    }

    /// A fake `filigree-mcp`: a newline-delimited JSON-RPC server (the transport
    /// the real one speaks). `argv[1]` selects a mode; `argv[2]` (optional) is a
    /// sidecar the tool-call arguments are dumped to. `tools/call` replies with a
    /// `result.content[0].text` envelope — exactly what `run_mcp_tool` extracts.
    const FAKE_FILIGREE_MCP_PY: &str = r#"
import sys, json, time
mode = sys.argv[1] if len(sys.argv) > 1 else "ok"
sidecar = sys.argv[2] if len(sys.argv) > 2 else None

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method, rid = req.get("method"), req.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": "2025-11-25",
            "serverInfo": {"name": "fake-filigree", "version": "0"},
            "capabilities": {"tools": {}}}})
    elif method == "tools/call":
        args = (req.get("params") or {}).get("arguments") or {}
        if sidecar:
            with open(sidecar, "w") as f:
                json.dump(args, f)
        if mode == "hang":
            time.sleep(60); continue
        env = {"ok": True, "observation_id": "filigree-obs-1", "summary": args.get("summary")}
        send({"jsonrpc": "2.0", "id": rid,
              "result": {"content": [{"type": "text", "text": json.dumps(env)}]}})
    elif rid is not None:
        send({"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "unknown"}})
    else:
        send({"jsonrpc": "2.0", "id": None, "error": {"code": -32601, "message": "unknown"}})
"#;

    fn write_fake_filigree(dir: &Path) -> PathBuf {
        let script = dir.join("fake_filigree_mcp.py");
        std::fs::write(&script, FAKE_FILIGREE_MCP_PY).expect("write fake filigree mcp");
        script
    }

    /// The transport regression: over the REAL newline-delimited subprocess
    /// transport, an `observation_create` call completes and the envelope parses.
    /// (The bug: Content-Length framing made filigree-mcp error and the read hang.)
    #[test]
    fn real_transport_completes_observation_call_over_newline_jsonrpc() {
        let dir = tempfile::tempdir().expect("temp dir");
        let script = write_fake_filigree(dir.path());
        let sidecar = dir.path().join("args.json");
        let value = run_mcp_tool_over_command(
            "python3",
            &[
                script.display().to_string(),
                "ok".to_owned(),
                sidecar.display().to_string(),
            ],
            Some(dir.path()),
            Duration::from_secs(10),
            "observation_create",
            &serde_json::json!({"summary": "hello", "priority": 3}),
        )
        .expect("observation_create completes over the newline transport");
        assert_eq!(value["ok"], serde_json::json!(true));
        assert_eq!(value["observation_id"], serde_json::json!("filigree-obs-1"));

        let args: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).expect("sidecar"))
                .expect("sidecar JSON");
        assert_eq!(args["summary"], serde_json::json!("hello"));
    }

    /// A filigree-mcp that completes the handshake then never answers the call
    /// must DEGRADE via the bounded timeout, not hang forever.
    #[test]
    fn real_transport_times_out_instead_of_hanging() {
        let dir = tempfile::tempdir().expect("temp dir");
        let script = write_fake_filigree(dir.path());
        let start = std::time::Instant::now();
        let err = run_mcp_tool_over_command(
            "python3",
            &[script.display().to_string(), "hang".to_owned()],
            Some(dir.path()),
            Duration::from_secs(1),
            "observation_create",
            &serde_json::json!({"summary": "hello"}),
        )
        .expect_err("a hung filigree-mcp must error, not hang");
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "must return promptly via the timeout"
        );
        assert!(matches!(err, FiligreeClientError::McpTool { .. }), "{err}");
        assert!(
            err.to_string().contains("did not respond"),
            "the timeout reason is surfaced: {err}"
        );
    }
}
