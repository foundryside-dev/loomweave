//! Filigree HTTP contract helpers for Clarion MCP.

use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

use crate::config::FiligreeConfig;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EntityAssociationsResponse {
    pub associations: Vec<EntityAssociation>,
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

    #[error(transparent)]
    Contract(#[from] FiligreeContractError),
}

pub trait FiligreeLookup: Send + Sync {
    fn associations_for(
        &self,
        entity_id: &str,
    ) -> Result<EntityAssociationsResponse, FiligreeClientError>;
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
}

pub fn parse_entity_associations_response(
    body: &str,
) -> Result<EntityAssociationsResponse, FiligreeContractError> {
    serde_json::from_str(body).map_err(FiligreeContractError::from)
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
}
