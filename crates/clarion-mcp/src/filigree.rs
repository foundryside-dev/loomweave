//! Filigree HTTP contract helpers for Clarion MCP.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::FiligreeConfig;
use crate::scan_results::{
    ScanResultsRequest, ScanResultsResponse, parse_scan_results_response, scan_results_url,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EntityAssociation {
    pub issue_id: String,
    pub clarion_entity_id: String,
    pub content_hash_at_attach: String,
    pub attached_at: String,
    pub attached_by: String,
}

#[derive(Debug, Error)]
pub enum FiligreeContractError {
    #[error("invalid Filigree entity association response: {0}")]
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
}

#[derive(Debug, Clone)]
pub struct FiligreeHttpClient {
    base_url: String,
    actor: String,
    token: Option<String>,
    client: reqwest::blocking::Client,
}

impl FiligreeHttpClient {
    pub fn from_config<F>(
        config: &FiligreeConfig,
        env_lookup: F,
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
}

impl FiligreeLookup for FiligreeHttpClient {
    fn associations_for(
        &self,
        entity_id: &str,
    ) -> Result<EntityAssociationsResponse, FiligreeClientError> {
        let mut request = self
            .client
            .get(entity_associations_url(&self.base_url, entity_id))
            .header("accept", "application/json");
        if !self.actor.trim().is_empty() {
            request = request.header("x-filigree-actor", self.actor.as_str());
        }
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().map_err(FiligreeClientError::Request)?;
        let status = response.status();
        let body = response.text().map_err(FiligreeClientError::Request)?;
        if !status.is_success() {
            return Err(FiligreeClientError::HttpStatus {
                status: status.as_u16(),
                body,
            });
        }
        parse_entity_associations_response(&body).map_err(FiligreeClientError::from)
    }

    fn issue_detail(&self, issue_id: &str) -> Result<Option<IssueDetail>, FiligreeClientError> {
        let mut request = self
            .client
            .get(issue_detail_url(&self.base_url, issue_id))
            .header("accept", "application/json");
        if !self.actor.trim().is_empty() {
            request = request.header("x-filigree-actor", self.actor.as_str());
        }
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let response = request.send().map_err(FiligreeClientError::Request)?;
        let status = response.status();
        // A 404 means the issue (or the whole detail route) is absent — the
        // enrich-only degrade signal, not an error.
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
        parse_issue_detail_response(&body)
            .map(Some)
            .map_err(FiligreeClientError::from)
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
        };
        let client = FiligreeHttpClient::from_config(&config, |name| {
            (name == "TEST_FILIGREE_TOKEN").then(|| "secret-token".to_owned())
        })
        .expect("build client")
        .expect("enabled client");

        let row = clarion_storage::FindingForEmitRow {
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

    fn detail_test_client(addr: std::net::SocketAddr) -> FiligreeHttpClient {
        let config = FiligreeConfig {
            enabled: true,
            base_url: format!("http://{addr}"),
            actor: "clarion-test".to_owned(),
            token_env: "TEST_FILIGREE_TOKEN".to_owned(),
            timeout_seconds: 1,
            emit_findings: true,
        };
        FiligreeHttpClient::from_config(&config, |_| None)
            .expect("build client")
            .expect("enabled client")
    }
}
