//! Stable Entity Identity (SEI) resolution and lineage endpoints (ADR-038).
//!
//! Split out of `http_read.rs` (mechanical relocation; behaviour unchanged).

use anyhow::Result;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use loomweave_core::HttpErrorCode as ErrorCode;
use loomweave_storage::{
    SeiLookupResult, StorageError, is_reserved_sei, resolve_locator, resolve_sei, sei_lineage,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::errors::json_read_error;
use super::{AppState, json_error};

/// Max locators in one `resolve:batch` request (mirrors `BATCH_MAX_QUERIES`).
pub(crate) const IDENTITY_BATCH_MAX: usize = 256;

/// Closed failure vocabulary for the producer reference ownership check.
/// Consumers run the equivalent check before issuing catalogue SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnershipValidationError {
    MalformedLocalInstanceId,
    MalformedCapabilityOwnership,
    MalformedIdentityOwnership,
    CapabilityApiVersionMismatch,
    IdentityApiVersionMismatch,
    CapabilityInstanceMismatch,
    IdentityInstanceMismatch,
}

/// Reference implementation of the project-ownership join guard.
///
/// Both the unauthenticated capability probe and the protected identity body
/// must identify this API version and the caller's local project instance.
/// This helper inspects only response JSON and never opens the catalogue.
pub(crate) fn validate_response_ownership(
    local_instance_id: &str,
    capability_body: &serde_json::Value,
    identity_body: &serde_json::Value,
) -> std::result::Result<(), OwnershipValidationError> {
    let local = Uuid::parse_str(local_instance_id)
        .map_err(|_| OwnershipValidationError::MalformedLocalInstanceId)?;
    let (capability_api, capability_instance) = response_ownership(capability_body)
        .ok_or(OwnershipValidationError::MalformedCapabilityOwnership)?;
    let (identity_api, identity_instance) = response_ownership(identity_body)
        .ok_or(OwnershipValidationError::MalformedIdentityOwnership)?;

    if capability_api != super::HTTP_API_VERSION {
        return Err(OwnershipValidationError::CapabilityApiVersionMismatch);
    }
    if identity_api != super::HTTP_API_VERSION {
        return Err(OwnershipValidationError::IdentityApiVersionMismatch);
    }
    if capability_instance != local {
        return Err(OwnershipValidationError::CapabilityInstanceMismatch);
    }
    if identity_instance != local {
        return Err(OwnershipValidationError::IdentityInstanceMismatch);
    }
    Ok(())
}

fn response_ownership(body: &serde_json::Value) -> Option<(u8, Uuid)> {
    let api_version = u8::try_from(body.get("api_version")?.as_u64()?).ok()?;
    let instance_id = Uuid::parse_str(body.get("instance_id")?.as_str()?).ok()?;
    Some((api_version, instance_id))
}

fn owned_identity_body(state: &AppState, payload: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(mut object) = payload else {
        panic!("identity response payloads are JSON objects");
    };
    object.insert(
        "api_version".to_owned(),
        serde_json::Value::from(super::HTTP_API_VERSION),
    );
    object.insert(
        "instance_id".to_owned(),
        serde_json::Value::String(state.instance_id.to_string()),
    );
    let body = serde_json::Value::Object(object);
    let capability_ownership = serde_json::json!({
        "api_version": super::HTTP_API_VERSION,
        "instance_id": state.instance_id,
    });
    debug_assert_eq!(
        validate_response_ownership(&state.instance_id.to_string(), &capability_ownership, &body,),
        Ok(())
    );
    body
}

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
        return Err("not a valid locator: input is an SEI (reserved loomweave:eid: prefix)");
    }
    let segments: Vec<&str> = locator.splitn(3, ':').collect();
    if segments.len() != 3 || segments.iter().any(|s| s.is_empty()) {
        return Err("not a valid locator: expected {plugin}:{kind}:{qualname}");
    }
    Ok(())
}

pub(crate) fn lineage_rows_to_body(
    rows: Vec<loomweave_storage::SeiLineageRow>,
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
            Json(owned_identity_body(
                &state,
                serde_json::json!({
                    "sei": record.sei,
                    "current_locator": record.current_locator,
                    "content_hash": record.content_hash,
                    "alive": true,
                }),
            )),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::OK,
            Json(owned_identity_body(
                &state,
                serde_json::json!({
                    "alive": false,
                }),
            )),
        )
            .into_response(),
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
            Json(owned_identity_body(
                &state,
                serde_json::json!({
                    "resolved": resolved,
                    "invalid": invalid,
                    "not_found": not_found,
                }),
            )),
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
            Json(owned_identity_body(
                &state,
                serde_json::json!({
                    "sei": sei,
                    "current_locator": record.current_locator,
                    "content_hash": record.content_hash,
                    "alive": true,
                }),
            )),
        )
            .into_response(),
        Ok(SeiLookupResult::NotAlive { lineage }) => (
            StatusCode::OK,
            Json(owned_identity_body(
                &state,
                serde_json::json!({
                    "sei": sei,
                    "alive": false,
                    "lineage": lineage_rows_to_body(lineage),
                }),
            )),
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
            Json(owned_identity_body(
                &state,
                serde_json::json!({
                    "sei": sei,
                    "lineage": lineage_rows_to_body(rows),
                }),
            )),
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
        let err = validate_locator("loomweave:eid:0123456789abcdef0123456789abcdef")
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

    #[test]
    fn ownership_validator_accepts_only_capability_and_response_for_local_project() {
        let local = "9bd7234e-6d44-4a38-9ae4-76f912a10221";
        let matching = serde_json::json!({
            "api_version": 1,
            "instance_id": local,
        });
        assert_eq!(
            validate_response_ownership(local, &matching, &matching),
            Ok(())
        );

        let other = serde_json::json!({
            "api_version": 1,
            "instance_id": "00000000-0000-4000-8000-000000000099",
        });
        assert_eq!(
            validate_response_ownership(local, &matching, &other),
            Err(OwnershipValidationError::IdentityInstanceMismatch)
        );
        assert_eq!(
            validate_response_ownership(local, &other, &matching),
            Err(OwnershipValidationError::CapabilityInstanceMismatch)
        );
    }

    #[test]
    fn ownership_validator_rejects_malformed_or_wrong_api_ownership() {
        let local = "9bd7234e-6d44-4a38-9ae4-76f912a10221";
        let valid = serde_json::json!({"api_version": 1, "instance_id": local});
        let malformed = serde_json::json!({"api_version": "1", "instance_id": local});
        assert_eq!(
            validate_response_ownership(local, &malformed, &valid),
            Err(OwnershipValidationError::MalformedCapabilityOwnership)
        );

        let wrong_api = serde_json::json!({"api_version": 2, "instance_id": local});
        assert_eq!(
            validate_response_ownership(local, &valid, &wrong_api),
            Err(OwnershipValidationError::IdentityApiVersionMismatch)
        );

        let malformed_uuid = serde_json::json!({"api_version": 1, "instance_id": "not-a-uuid"});
        assert_eq!(
            validate_response_ownership(local, &malformed_uuid, &valid),
            Err(OwnershipValidationError::MalformedCapabilityOwnership)
        );
    }
}
