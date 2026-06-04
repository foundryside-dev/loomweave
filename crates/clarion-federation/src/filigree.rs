//! Filigree HTTP/MCP contract helpers for Clarion MCP.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use clarion_core::plugin::{ContentLengthCeiling, Frame, read_frame, write_frame};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::FiligreeConfig;
use crate::scan_results::{
    CleanStaleRequest, CleanStaleResponse, ScanResultsRequest, ScanResultsResponse,
    clean_stale_url, parse_clean_stale_response, parse_scan_results_response, scan_results_url,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EntityAssociationsResponse {
    pub associations: Vec<EntityAssociation>,
}

/// The subset of a Filigree issue Clarion surfaces alongside an
/// entity-association match: enough to render the match without an agent
/// having to call back into Filigree. Sourced from `GET /api/loom/issues/{id}`.
/// Unknown fields in the response are ignored, so Filigree can grow the route
/// without breaking this read.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct IssueDetail {
    pub title: String,
    pub status: String,
    pub priority: i64,
}

/// Request Clarion sends to Filigree's observation scratchpad when an agent
/// proposes guidance. This is an observation, not a Clarion sheet.
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

/// Pending Filigree observation row, as read from `GET /api/loom/observations`
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
    pub clarion_entity_id: String,
    pub content_hash_at_attach: String,
    pub attached_at: String,
    pub attached_by: String,
}

/// One Wardline finding as Clarion surfaces it — the subset of Filigree's
/// `ScanFindingLoom` (`GET /api/loom/findings`) used for read-time
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

/// Envelope returned by `GET /api/loom/findings` — the paged list of
/// [`WardlineFinding`] rows Clarion reconciles against.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WardlineFindingsResponse {
    #[serde(default)]
    pub items: Vec<WardlineFinding>,
    /// True when more findings pages follow. Clarion does not page the findings
    /// list (the offset param is unpinned in the federation contract); when this
    /// is true the first page is an incomplete view, so the caller fails closed
    /// to `unavailable` rather than silently undercounting the file's findings.
    #[serde(default)]
    pub has_more: bool,
}

/// One row of `GET /api/loom/files` — only the fields needed to map a path to
/// Filigree's `file_id`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LoomFileRecord {
    pub file_id: String,
    pub path: String,
}

/// Envelope returned by `GET /api/loom/files` — the paged list of
/// [`LoomFileRecord`] rows Clarion uses to map a path to a `file_id`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LoomFilesResponse {
    #[serde(default)]
    pub items: Vec<LoomFileRecord>,
    /// True when more pages follow. When the exact-path match is absent and
    /// `has_more` is true, the result is indeterminate — the file may be on a
    /// later page — so callers must degrade to `unavailable` rather than
    /// concluding `no_matches`.
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LoomObservationsResponse {
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

pub fn parse_loom_files_response(body: &str) -> Result<LoomFilesResponse, FiligreeContractError> {
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

    /// Mark a pending observation as consumed after Clarion writes the local
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

#[derive(Debug, Clone)]
pub struct FiligreeHttpClient {
    base_url: String,
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
        let token = env_lookup(&config.token_env).filter(|value| !value.trim().is_empty());
        Ok(Some(Self {
            base_url: config.base_url.clone(),
            actor: config.actor.clone(),
            token,
            client,
            project_root: project_root.map(Path::to_path_buf),
        }))
    }

    /// POST a scan-results batch to Filigree's native intake (WP9-B,
    /// REQ-FINDING-03). One-way Clarion→Filigree push; the caller is expected to
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
            .post(scan_results_url(&self.base_url))
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
    /// `--prune-unseen`). One-way Clarion→Filigree call; Filigree soft-archives
    /// its own `unseen_in_latest` findings for the given `scan_source`. The
    /// `scan_source` scoping is enforced server-side, so this can only sweep
    /// Clarion's findings.
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
            .map_err(|err| FiligreeClientError::McpTool {
                tool: tool.to_owned(),
                message: format!("spawn {program}: {err}"),
            })?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| FiligreeClientError::McpTool {
                tool: tool.to_owned(),
                message: "child stdin unavailable".to_owned(),
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| FiligreeClientError::McpTool {
                tool: tool.to_owned(),
                message: "child stdout unavailable".to_owned(),
            })?;
        let mut stdout = BufReader::new(stdout);

        write_mcp_json(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "clarion-init",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "clarion",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }
            }),
            tool,
        )?;
        let _ = read_mcp_json(&mut stdout, "clarion-init", tool)?;

        write_mcp_json(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
            tool,
        )?;

        write_mcp_json(
            &mut stdin,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "clarion-call",
                "method": "tools/call",
                "params": {
                    "name": tool,
                    "arguments": arguments,
                }
            }),
            tool,
        )?;
        drop(stdin);

        let response = read_mcp_json(&mut stdout, "clarion-call", tool)?;
        let _ = child.wait();
        if let Some(error) = response.get("error") {
            return Err(FiligreeClientError::McpTool {
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
            .ok_or_else(|| FiligreeClientError::McpTool {
                tool: tool.to_owned(),
                message: format!("missing result.content[0].text in response {response}"),
            })?;
        let parsed: serde_json::Value =
            serde_json::from_str(text).map_err(FiligreeClientError::InvalidObservationResponse)?;
        if parsed.get("error").is_some() {
            return Err(FiligreeClientError::McpTool {
                tool: tool.to_owned(),
                message: parsed.to_string(),
            });
        }
        Ok(parsed)
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
        let files: LoomFilesResponse =
            self.get_json(&loom_files_url(&self.base_url, "wardline", path))?;
        let exact = files.items.into_iter().find(|f| f.path == path);
        let Some(file_id) = exact.map(|f| f.file_id) else {
            // No exact match on this page. If has_more is true the result is
            // indeterminate — the file may be on a later page — so degrade to
            // unavailable rather than falsely concluding no_matches.
            if files.has_more {
                return Err(FiligreeClientError::HttpStatus {
                    status: 0,
                    body:
                        "loom/files truncated before exact path match; cannot conclude no findings"
                            .to_owned(),
                });
            }
            return Ok(Vec::new());
        };
        // Hop 2: file_id -> wardline findings. As with hop-1, Clarion reads only
        // the first page; if it is truncated (`has_more`) the findings view is
        // incomplete, so fail closed to `unavailable` rather than returning a
        // silent undercount.
        let findings: WardlineFindingsResponse =
            self.get_json(&loom_findings_url(&self.base_url, "wardline", &file_id))?;
        if findings.has_more {
            return Err(FiligreeClientError::HttpStatus {
                status: 0,
                body: "loom/findings truncated; cannot enumerate all findings for file".to_owned(),
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
            let page: LoomObservationsResponse =
                self.get_json(&loom_observations_url(&self.base_url, limit, offset))?;
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
        "{}/api/loom/issues/{}",
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

pub fn loom_files_url(base_url: &str, scan_source: &str, path_prefix: &str) -> String {
    format!(
        "{}/api/loom/files?scan_source={}&path_prefix={}",
        base_url.trim_end_matches('/'),
        percent_encode_query_value(scan_source),
        percent_encode_query_value(path_prefix)
    )
}

pub fn loom_findings_url(base_url: &str, scan_source: &str, file_id: &str) -> String {
    format!(
        "{}/api/loom/findings?scan_source={}&file_id={}",
        base_url.trim_end_matches('/'),
        percent_encode_query_value(scan_source),
        percent_encode_query_value(file_id)
    )
}

pub fn loom_observations_url(base_url: &str, limit: u64, offset: u64) -> String {
    format!(
        "{}/api/loom/observations?limit={}&offset={}",
        base_url.trim_end_matches('/'),
        limit,
        offset
    )
}

fn write_mcp_json(
    writer: &mut impl Write,
    value: &serde_json::Value,
    tool: &str,
) -> Result<(), FiligreeClientError> {
    let body = serde_json::to_vec(value).map_err(|err| FiligreeClientError::McpTool {
        tool: tool.to_owned(),
        message: format!("serialize MCP request: {err}"),
    })?;
    write_frame(writer, &Frame { body }).map_err(|err| FiligreeClientError::McpTool {
        tool: tool.to_owned(),
        message: format!("write MCP frame: {err}"),
    })
}

fn read_mcp_json(
    reader: &mut impl std::io::BufRead,
    expected_id: &str,
    tool: &str,
) -> Result<serde_json::Value, FiligreeClientError> {
    loop {
        let frame = read_frame(reader, ContentLengthCeiling::DEFAULT).map_err(|err| {
            FiligreeClientError::McpTool {
                tool: tool.to_owned(),
                message: format!("read MCP frame: {err}"),
            }
        })?;
        let value: serde_json::Value =
            serde_json::from_slice(&frame.body).map_err(|err| FiligreeClientError::McpTool {
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

fn resolve_filigree_mcp_command(project_root: Option<&Path>) -> (String, Vec<String>) {
    if let Ok(raw) = std::env::var("CLARION_FILIGREE_MCP_COMMAND") {
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

    ("filigree".to_owned(), vec!["mcp".to_owned()])
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

    #[test]
    fn parses_reverse_entity_association_response_shape() {
        let parsed = parse_entity_associations_response(
            r#"{
                "associations": [
                    {
                        "issue_id": "filigree-1234567890",
                        "clarion_entity_id": "python:function:demo.hello",
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
        assert_eq!(row.clarion_entity_id, "python:function:demo.hello");
        assert_eq!(row.content_hash_at_attach, "hash-a");
        assert_eq!(row.attached_at, "2026-05-17T00:00:00.000Z");
        assert_eq!(row.attached_by, "codex");
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
            assert!(request.contains("x-filigree-actor: clarion-test"));
            assert!(request.contains("authorization: Bearer secret-token"));

            let body = r#"{"associations":[{"issue_id":"filigree-1234567890","clarion_entity_id":"python:function:demo.hello","content_hash_at_attach":"hash-a","attached_at":"2026-05-17T00:00:00.000Z","attached_by":"codex"}]}"#;
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
            actor: "clarion-test".to_owned(),
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
            "http://127.0.0.1:8542/api/loom/issues/clarion-51a2868c86"
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
            assert!(request.contains("GET /api/loom/issues/clarion-51a2868c86 HTTP/1.1"));

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
            .issue_detail("clarion-missing")
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
            assert!(request.contains("x-filigree-actor: clarion-test"));
            assert!(request.contains("authorization: Bearer secret-token"));
            // The wire body carries the mapped severity, not the internal one.
            assert!(
                request.contains("\"scan_source\":\"clarion\""),
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
            actor: "clarion-test".to_owned(),
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
            id: "core:finding:run-1:circular".to_owned(),
            rule_id: "CLA-PY-STRUCTURE-001".to_owned(),
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
    fn parses_loom_findings_list_envelope() {
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
    fn parses_loom_files_list_envelope() {
        let resp = parse_loom_files_response(
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
    fn builds_loom_url_builders_with_encoding() {
        assert_eq!(
            loom_files_url("http://127.0.0.1:8542/", "wardline", "src/demo.py"),
            "http://127.0.0.1:8542/api/loom/files?scan_source=wardline&path_prefix=src%2Fdemo.py"
        );
        assert_eq!(
            loom_findings_url("http://127.0.0.1:8542/", "wardline", "file-9"),
            "http://127.0.0.1:8542/api/loom/findings?scan_source=wardline&file_id=file-9"
        );
    }

    #[test]
    fn wardline_findings_for_path_does_two_hops_and_exact_path_filter() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let handle = std::thread::spawn(move || {
            // Hop 1: GET /api/loom/files — path_prefix matches two files; the
            // exact-path filter must pick file-9, not the helpers file.
            let (mut s1, _) = listener.accept().expect("accept files");
            let mut buf = [0_u8; 4096];
            let n = s1.read(&mut buf).expect("read files req");
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(req.contains(
                "GET /api/loom/files?scan_source=wardline&path_prefix=src%2Fdemo.py HTTP/1.1"
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

            // Hop 2: GET /api/loom/findings for file-9.
            let (mut s2, _) = listener.accept().expect("accept findings");
            let n = s2.read(&mut buf).expect("read findings req");
            let req = String::from_utf8_lossy(&buf[..n]);
            assert!(
                req.contains("GET /api/loom/findings?scan_source=wardline&file_id=file-9 HTTP/1.1")
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
            actor: "clarion-test".to_owned(),
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
                "GET /api/loom/files?scan_source=wardline&path_prefix=src%2Fdemo.py HTTP/1.1"
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
            actor: "clarion-test".to_owned(),
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
                "GET /api/loom/files?scan_source=wardline&path_prefix=src%2Fdemo.py HTTP/1.1"
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
                req.contains("GET /api/loom/findings?scan_source=wardline&file_id=file-9 HTTP/1.1")
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
            actor: "clarion-test".to_owned(),
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
            actor: "clarion-test".to_owned(),
            token_env: "TEST_FILIGREE_TOKEN".to_owned(),
            timeout_seconds: 1,
            emit_findings: true,
            prune_unseen_days: 30,
        };
        FiligreeHttpClient::from_config(&config, |_| None)
            .expect("build client")
            .expect("enabled client")
    }
}
