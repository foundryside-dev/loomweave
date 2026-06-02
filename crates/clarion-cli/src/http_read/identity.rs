//! Stable Entity Identity (SEI) resolution and lineage endpoints (ADR-038).
//!
//! Split out of `http_read.rs` (mechanical relocation; behaviour unchanged).

use anyhow::Result;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use clarion_core::HttpErrorCode as ErrorCode;
use clarion_storage::{
    SeiLookupResult, StorageError, is_reserved_sei, resolve_locator, resolve_sei, sei_lineage,
};
use serde::{Deserialize, Serialize};

use super::errors::json_read_error;
use super::{AppState, json_error};

/// Max locators in one `resolve:batch` request (mirrors `BATCH_MAX_QUERIES`).
pub(crate) const IDENTITY_BATCH_MAX: usize = 256;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolveLocatorRequest {
    locator: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolveLocatorBatchRequest {
    locators: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct SeiLineageEventBody {
    event: String,
    old_locator: Option<String>,
    new_locator: Option<String>,
    run_id: String,
    recorded_at: String,
}

/// Validate a locator for `resolve` (REQ-F-02). Rejects the reserved SEI prefix
/// and anything that is not a 3-segment `{plugin}:{kind}:{qualname}` with no
/// empty segment. Returns the documented client message on rejection.
pub(crate) fn validate_locator(locator: &str) -> Result<(), &'static str> {
    if is_reserved_sei(locator) {
        return Err("not a valid locator: input is an SEI (reserved clarion:eid: prefix)");
    }
    let segments: Vec<&str> = locator.splitn(3, ':').collect();
    if segments.len() != 3 || segments.iter().any(|s| s.is_empty()) {
        return Err("not a valid locator: expected {plugin}:{kind}:{qualname}");
    }
    Ok(())
}

pub(crate) fn lineage_rows_to_body(
    rows: Vec<clarion_storage::SeiLineageRow>,
) -> Vec<SeiLineageEventBody> {
    rows.into_iter()
        .map(|r| SeiLineageEventBody {
            event: r.event,
            old_locator: r.old_locator,
            new_locator: r.new_locator,
            run_id: r.run_id,
            recorded_at: r.recorded_at,
        })
        .collect()
}

pub(crate) async fn post_identity_resolve(
    State(state): State<AppState>,
    body: Result<Json<ResolveLocatorRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "request body must be a JSON object {\"locator\": \"...\"}",
        );
    };
    if let Err(message) = validate_locator(&request.locator) {
        return json_error(StatusCode::BAD_REQUEST, ErrorCode::InvalidPath, message);
    }
    let locator = request.locator;
    let result = state
        .readers
        .with_reader(move |conn| resolve_locator(conn, &locator))
        .await;
    match result {
        Ok(Some(record)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "sei": record.sei,
                "current_locator": record.current_locator,
                "content_hash": record.content_hash,
                "alive": true,
            })),
        )
            .into_response(),
        Ok(None) => (StatusCode::OK, Json(serde_json::json!({ "alive": false }))).into_response(),
        Err(err) => json_read_error(&err),
    }
}

pub(crate) async fn post_identity_resolve_batch(
    State(state): State<AppState>,
    body: Result<Json<ResolveLocatorBatchRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "request body must be a JSON object {\"locators\": [...]}",
        );
    };
    if request.locators.len() > IDENTITY_BATCH_MAX {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::BatchTooLarge,
            "locators[] exceeds the per-batch maximum of 256 entries",
        );
    }
    let locators = request.locators;
    let result = state
        .readers
        .with_reader(move |conn| {
            // BTreeMap → deterministic key order. Invalid (SEI-shaped or
            // malformed) inputs are collected separately, never mis-resolved.
            let mut resolved: std::collections::BTreeMap<String, serde_json::Value> =
                std::collections::BTreeMap::new();
            let mut invalid: Vec<String> = Vec::new();
            let mut not_found: Vec<String> = Vec::new();
            for locator in locators {
                if validate_locator(&locator).is_err() {
                    invalid.push(locator);
                    continue;
                }
                match resolve_locator(conn, &locator)? {
                    Some(record) => {
                        resolved.insert(
                            locator,
                            serde_json::json!({
                                "sei": record.sei,
                                "current_locator": record.current_locator,
                                "content_hash": record.content_hash,
                                "alive": true,
                            }),
                        );
                    }
                    None => not_found.push(locator),
                }
            }
            Ok::<_, StorageError>((resolved, invalid, not_found))
        })
        .await;
    match result {
        Ok((resolved, invalid, not_found)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "resolved": resolved,
                "invalid": invalid,
                "not_found": not_found,
            })),
        )
            .into_response(),
        Err(err) => json_read_error(&err),
    }
}

pub(crate) async fn get_identity_sei(
    State(state): State<AppState>,
    Path(sei): Path<String>,
) -> Response {
    let lookup_sei = sei.clone();
    let result = state
        .readers
        .with_reader(move |conn| resolve_sei(conn, &lookup_sei))
        .await;
    match result {
        Ok(SeiLookupResult::Alive(record)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "sei": sei,
                "current_locator": record.current_locator,
                "content_hash": record.content_hash,
                "alive": true,
            })),
        )
            .into_response(),
        Ok(SeiLookupResult::NotAlive { lineage }) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "sei": sei,
                "alive": false,
                "lineage": lineage_rows_to_body(lineage),
            })),
        )
            .into_response(),
        Err(err) => json_read_error(&err),
    }
}

pub(crate) async fn get_identity_lineage(
    State(state): State<AppState>,
    Path(sei): Path<String>,
) -> Response {
    let lookup_sei = sei.clone();
    let result = state
        .readers
        .with_reader(move |conn| sei_lineage(conn, &lookup_sei))
        .await;
    match result {
        Ok(rows) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "sei": sei,
                "lineage": lineage_rows_to_body(rows),
            })),
        )
            .into_response(),
        Err(err) => json_read_error(&err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_locator_rejects_reserved_sei_prefix() {
        // A real SEI has the same colon count as a locator — only the reserved
        // prefix distinguishes it, which is exactly what the rejection keys on.
        let err = validate_locator("clarion:eid:0123456789abcdef0123456789abcdef")
            .expect_err("an SEI-shaped input must be rejected");
        assert!(err.contains("not a valid locator"), "message: {err}");
    }

    #[test]
    fn validate_locator_rejects_malformed_locators() {
        assert!(validate_locator("python:function").is_err(), "two segments");
        assert!(validate_locator("python::qualname").is_err(), "empty kind");
        assert!(validate_locator("::").is_err(), "all empty");
        assert!(validate_locator("").is_err(), "empty string");
    }

    #[test]
    fn validate_locator_accepts_well_formed_locator() {
        assert!(validate_locator("python:function:auth.tokens.refresh").is_ok());
        // A qualname containing colons is fine (splitn(3) keeps the tail intact).
        assert!(validate_locator("python:function:a.b::c").is_ok());
    }
}
