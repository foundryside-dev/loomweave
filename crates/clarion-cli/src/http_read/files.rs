//! File-read endpoints (`/api/v1/files`, `:resolve`, `/batch`) and their DTOs.
//!
//! Split out of `http_read.rs` (mechanical relocation; behaviour unchanged).

use anyhow::Result;
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use clarion_core::HttpErrorCode as ErrorCode;
use clarion_storage::{CanonicalProjectPath, StorageError, resolve_file_catalog_entry};
use serde::{Deserialize, Serialize};

use super::errors::{classify_read_error, json_read_error, log_briefing_blocked_refusal};
use super::{AppState, ErrorResponse, json_error};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    language: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct FileResponse {
    entity_id: String,
    content_hash: String,
    canonical_path: CanonicalProjectPath,
    language: String,
}

/// Maximum number of `BatchFileQuery` entries a single
/// `POST /api/v1/files/batch` request may carry. Pinned in the federation
/// contract; Filigree splits oversize lookup sets client-side. Lifted to a
/// constant so the contract docs, the validator, and tests all point at
/// the same number.
pub(crate) const BATCH_MAX_QUERIES: usize = 256;

pub(crate) const RESOLVE_MAX_PATHS: usize = 1000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchFileQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    language: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchFileRequest {
    queries: Vec<BatchFileQuery>,
}

#[derive(Debug, Serialize)]
pub(crate) struct BatchResolvedItem {
    requested_path: String,
    entity_id: String,
    content_hash: String,
    canonical_path: CanonicalProjectPath,
    language: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct BatchErrorItem {
    requested_path: String,
    code: ErrorCode,
    message: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct BatchFileResponse {
    resolved: Vec<BatchResolvedItem>,
    not_found: Vec<String>,
    briefing_blocked: Vec<String>,
    errors: Vec<BatchErrorItem>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolveFileQuery {
    #[serde(default)]
    path: String,
    #[serde(default)]
    language: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolveFilesRequest {
    paths: Vec<ResolveFileQuery>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResolveFilesResponse {
    results: Vec<ResolveFileResult>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResolveFileResult {
    path: String,
    response: ResolveFileItemResponse,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResolveFileItemResponse {
    status: ResolveFileStatus,
    body: serde_json::Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResolveFileStatus {
    Resolved,
    NotFound,
    Blocked,
    Error,
}

pub(crate) async fn get_file(
    State(state): State<AppState>,
    headers: HeaderMap,
    query: Result<Query<FileQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "query parameters are invalid",
        );
    };
    if query.path.trim().is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "path query parameter must not be blank",
        );
    }
    let project_root = state.project_root.clone();
    let file_path = query.path;
    let language = query.language;
    let catalog_result = state
        .readers
        .with_reader(move |conn| {
            resolve_file_catalog_entry(conn, &project_root, &file_path, &language)
        })
        .await;
    let result = match catalog_result {
        Ok(Some(entry)) => entry.into_resolved_file().map(Some),
        Ok(None) => Ok(None),
        Err(err) => Err(err),
    };
    match result {
        Ok(Some(file)) => {
            if let Some(reason) = file.briefing_blocked.as_deref() {
                log_briefing_blocked_refusal(file.canonical_path.as_str(), reason);
                return json_error(
                    StatusCode::FORBIDDEN,
                    ErrorCode::BriefingBlocked,
                    "entity is briefing-blocked and cannot be exposed",
                );
            }
            let etag = file_etag(&file.content_hash);
            if if_none_match_matches(headers.get(header::IF_NONE_MATCH), &etag) {
                let mut response = StatusCode::NOT_MODIFIED.into_response();
                insert_etag(&mut response, &etag);
                return response;
            }
            let mut response = (
                StatusCode::OK,
                Json(FileResponse {
                    entity_id: file.entity_id,
                    content_hash: file.content_hash,
                    canonical_path: file.canonical_path,
                    language: file.language,
                }),
            )
                .into_response();
            insert_etag(&mut response, &etag);
            response
        }
        Ok(None) => json_error(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "file is not known to Clarion",
        ),
        Err(err) => json_read_error(&err),
    }
}

pub(crate) fn file_etag(content_hash: &str) -> String {
    format!("\"{content_hash}\"")
}

pub(crate) fn if_none_match_matches(value: Option<&HeaderValue>, etag: &str) -> bool {
    let Some(value) = value else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    value.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate == etag || candidate.strip_prefix("W/") == Some(etag)
    })
}

pub(crate) fn insert_etag(response: &mut Response, etag: &str) {
    if let Ok(value) = HeaderValue::from_str(etag) {
        response.headers_mut().insert(header::ETAG, value);
    }
}

/// Batch resolution endpoint. Resolves up to `BATCH_MAX_QUERIES` paths in a
/// single request, partitioning results into four lists:
///
/// - `resolved`        — paths that mapped to a file-kind entity.
/// - `not_found`       — paths Clarion does not have a catalog row for.
/// - `briefing_blocked` — paths whose entity carries a `briefing_blocked`
///   property (the partition equivalent of the single-file 403 surface).
/// - `errors`          — per-path resolution errors (`INVALID_PATH`,
///   `PATH_OUTSIDE_PROJECT`, `STORAGE_ERROR`, `INTERNAL`).
///
/// The whole batch runs inside **one** `with_reader` closure so we
/// check out one pooled connection per request, not one per query —
/// this is the perf win Filigree's `ClarionRegistry` needs for cold-
/// start hydration. `ETag` is intentionally not applied to the batch
/// surface; clients should `ETag` the single-file endpoint when they
/// want conditional fetch semantics.
pub(crate) async fn post_files_batch(
    State(state): State<AppState>,
    body: Result<Json<BatchFileRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "request body must be a JSON object {\"queries\": [...]}",
        );
    };
    if request.queries.len() > BATCH_MAX_QUERIES {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::BatchTooLarge,
            "queries[] exceeds the per-batch maximum of 256 entries",
        );
    }
    let project_root = state.project_root.clone();
    let queries = request.queries;
    let catalog_result = state
        .readers
        .with_reader(move |conn| {
            let mut resolved = Vec::new();
            let mut not_found = Vec::new();
            let mut briefing_blocked = Vec::new();
            let mut errors = Vec::new();
            for query in queries {
                if query.path.trim().is_empty() {
                    errors.push(BatchErrorItem {
                        requested_path: query.path.clone(),
                        code: ErrorCode::InvalidPath,
                        message: "path must not be blank".to_owned(),
                    });
                    continue;
                }
                match resolve_file_catalog_entry(conn, &project_root, &query.path, &query.language)
                {
                    Ok(Some(entry)) => match entry.into_resolved_file() {
                        Ok(file) => {
                            if file.briefing_blocked.is_some() {
                                briefing_blocked.push(query.path);
                            } else {
                                resolved.push(BatchResolvedItem {
                                    requested_path: query.path,
                                    entity_id: file.entity_id,
                                    content_hash: file.content_hash,
                                    canonical_path: file.canonical_path,
                                    language: file.language,
                                });
                            }
                        }
                        Err(err) => errors.push(classify_batch_error(query.path, &err)),
                    },
                    Ok(None) => not_found.push(query.path),
                    Err(err) => errors.push(classify_batch_error(query.path, &err)),
                }
            }
            Ok::<_, StorageError>(BatchFileResponse {
                resolved,
                not_found,
                briefing_blocked,
                errors,
            })
        })
        .await;
    match catalog_result {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => json_read_error(&err),
    }
}

pub(crate) async fn post_files_resolve(
    State(state): State<AppState>,
    body: Result<Json<ResolveFilesRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(request)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "request body must be a JSON object {\"paths\": [...]}",
        );
    };
    if request.paths.len() > RESOLVE_MAX_PATHS {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "paths[] exceeds the per-batch maximum of 1000 entries",
        );
    }
    let project_root = state.project_root.clone();
    let paths = request.paths;
    let catalog_result = state
        .readers
        .with_reader(move |conn| {
            let results = paths
                .into_iter()
                .map(|query| {
                    let response =
                        resolve_file_query_item(conn, &project_root, &query.path, &query.language);
                    ResolveFileResult {
                        path: query.path,
                        response,
                    }
                })
                .collect();
            Ok::<_, StorageError>(ResolveFilesResponse { results })
        })
        .await;
    match catalog_result {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(err) => json_read_error(&err),
    }
}

// ── Call-graph linkages (Wave 0 / WS2) ──────────────────────────────────────
//
// Thin HTTP wrappers over `clarion_storage::call_edges_targeting` (callers) and
// `call_edges_from` (callees). Aggregated per neighbour entity: `call_site_count`
// is the number of call sites (across all returned confidence tiers) and
// `confidence` is the STRONGEST tier present (resolved > ambiguous > inferred) —
// a real resolved site is reported as resolved even if weaker sites also exist.
// Inferred-tier results reflect only already-persisted inferred edges; the
// read-only HTTP surface does not materialise inferred edges on demand (that is
// an MCP, writer-backed capability). Routes are HMAC-gated like `/api/v1/files`.
//
// briefing-block policy: the QUERIED entity is refused with 403 when blocked
// (mirrors `get_file`, honouring the federation "refuse blocked entities to
// siblings" contract). NEIGHBOUR entities are NOT filtered — the linkage payload
// is structural topology (ids + counts + tier), consistent with the MCP
// call-graph surface (`callers_of`/`neighborhood`), which does not filter
// briefing-blocked neighbours either.

pub(crate) fn resolve_file_query_item(
    conn: &rusqlite::Connection,
    project_root: &std::path::Path,
    path: &str,
    language: &str,
) -> ResolveFileItemResponse {
    if path.trim().is_empty() {
        return resolve_error_response(
            ResolveFileStatus::Error,
            ErrorCode::InvalidPath,
            "path must not be blank",
        );
    }
    match resolve_file_catalog_entry(conn, project_root, path, language) {
        Ok(Some(entry)) => match entry.into_resolved_file() {
            Ok(file) => {
                if file.briefing_blocked.is_some() {
                    resolve_error_response(
                        ResolveFileStatus::Blocked,
                        ErrorCode::BriefingBlocked,
                        "entity is briefing-blocked and cannot be exposed",
                    )
                } else {
                    ResolveFileItemResponse {
                        status: ResolveFileStatus::Resolved,
                        body: serde_json::to_value(FileResponse {
                            entity_id: file.entity_id,
                            content_hash: file.content_hash,
                            canonical_path: file.canonical_path,
                            language: file.language,
                        })
                        .expect("FileResponse serializes"),
                    }
                }
            }
            Err(err) => resolve_read_error_response(&err),
        },
        Ok(None) => resolve_error_response(
            ResolveFileStatus::NotFound,
            ErrorCode::NotFound,
            "file is not known to Clarion",
        ),
        Err(err) => resolve_read_error_response(&err),
    }
}

pub(crate) fn resolve_read_error_response(err: &StorageError) -> ResolveFileItemResponse {
    let error = classify_read_error(err);
    resolve_error_response(ResolveFileStatus::Error, error.code, error.message)
}

pub(crate) fn resolve_error_response(
    status: ResolveFileStatus,
    code: ErrorCode,
    message: &str,
) -> ResolveFileItemResponse {
    ResolveFileItemResponse {
        status,
        body: serde_json::to_value(ErrorResponse {
            error: message.to_owned(),
            code,
        })
        .expect("ErrorResponse serializes"),
    }
}

pub(crate) fn classify_batch_error(requested_path: String, err: &StorageError) -> BatchErrorItem {
    let classified = classify_read_error(err);
    BatchErrorItem {
        requested_path,
        code: classified.code,
        message: classified.message.to_owned(),
    }
}
