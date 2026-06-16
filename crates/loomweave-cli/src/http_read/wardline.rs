//! Wardline taint-store endpoints (resolve / taint-facts read+write).
//!
//! Split out of `http_read.rs` (mechanical relocation; behaviour unchanged).

use anyhow::Result;
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use loomweave_core::HttpErrorCode as ErrorCode;
use loomweave_storage::StorageError;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::errors::{format_error_chain, iso8601_now, json_read_error};
use super::{AppState, HTTP_ERROR_DISPATCH, json_error};

/// Max qualnames/facts in one Wardline request.
pub(crate) const WARDLINE_TAINT_BATCH_MAX: usize = 2000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResolveRequest {
    #[serde(default)]
    project: String,
    qualnames: Vec<String>,
    /// Optional batch-scoped plugin hint (clarion-b1a158f7f5; ADR-036
    /// plugin-hint amendment; agreed shape in Wardline's
    /// `2026-06-11-wardline-resolve-plugin-hint-proposal.md`). An ADR-049
    /// plugin id (`python`, `rust`) restricting resolution to that plugin's
    /// namespace — a CONSTRAINT, never a preference order. Omission is legal
    /// forever and is byte-for-byte the pre-hint cross-plugin behavior.
    /// Adjudicated edge cases: blank/whitespace-only → 400 naming this field
    /// (cross-version diagnosability); any other non-blank string is honored
    /// as a constraint (an unknown plugin id simply resolves nothing — plugin
    /// ids are NOT validated against the store).
    #[serde(default)]
    plugin: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ResolveResponse {
    /// qualname -> `entity_id`, only for exact matches.
    resolved: std::collections::BTreeMap<String, String>,
    unresolved: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaintFactInput {
    qualname: String,
    /// `RawValue` captures the ORIGINAL bytes of this JSON sub-value exactly —
    /// `serde_json::Value` would normalize (object keys are a `BTreeMap`, so
    /// `{"b":2,"a":1}` would re-emit as `{"a":1,"b":2}`). The federation
    /// contract is "stored and returned verbatim", so we preserve the bytes.
    wardline_json: Box<serde_json::value::RawValue>,
    #[serde(default)]
    scan_id: Option<String>,
    #[serde(default)]
    content_hash_at_compute: Option<String>,
    /// Optional caller-supplied Stable Entity Identity (T3.4, migration 0006).
    /// Opaque — stored verbatim, never parsed. When omitted, the write path
    /// resolves the alive SEI for the resolved locator server-side. When both
    /// caller and server values are present, they must match.
    #[serde(default)]
    sei: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WriteTaintFactsRequest {
    #[serde(default)]
    project: String,
    #[serde(default)]
    scan_id: Option<String>,
    facts: Vec<TaintFactInput>,
}

#[derive(Debug, Serialize)]
pub(crate) struct WriteTaintFactsResponse {
    written: usize,
    unresolved_qualnames: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaintFactQuery {
    #[serde(default)]
    project: String,
    qualname: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct TaintFactView {
    qualname: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    wardline_json: Option<Box<serde_json::value::RawValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_content_hash: Option<String>,
    exists: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchGetRequest {
    #[serde(default)]
    project: String,
    qualnames: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchGetBySeiRequest {
    #[serde(default)]
    project: String,
    /// Opaque SEIs (`loomweave:eid:<hex>`). Treated verbatim — NO locator-shape
    /// validation (SEI-shaped strings are the valid input here, the inverse of
    /// the `resolve` REQ-F-02 rejection).
    seis: Vec<String>,
}

/// One taint fact keyed by SEI (T3.4 read-by-SEI surface). Same fields as
/// [`TaintFactView`] but keyed on the opaque `sei` instead of the qualname:
/// `exists: false` when no SEI-tagged fact is stored for the SEI.
#[derive(Debug, Serialize)]
pub(crate) struct TaintFactBySeiView {
    sei: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    wardline_json: Option<Box<serde_json::value::RawValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_content_hash: Option<String>,
    exists: bool,
}

/// Exact-tier Wardline qualname resolve (ADR-036, W.4). Takes a batch of
/// PRE-COMPOSED dotted qualnames that Wardline has already shaped to
/// byte-match Loomweave's `canonical_qualified_name`; resolution is the direct
/// existence lookup in `loomweave_storage::resolve_wardline_qualnames_for_plugin`,
/// optionally constrained by the batch-scoped `plugin` hint (see
/// [`ResolveRequest::plugin`]). No `&file=` disambiguator, no normalization —
/// the generic resolve oracle remains deferred.
pub(crate) async fn post_wardline_resolve(
    State(state): State<AppState>,
    body: Result<Json<ResolveRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(json) => json,
        Err(rej) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidPath,
                &rej.body_text(),
            );
        }
    };
    if let Some(resp) = state.reject_project_mismatch(&req.project) {
        return resp;
    }
    if req.qualnames.len() > WARDLINE_TAINT_BATCH_MAX {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::BatchTooLarge,
            "too many qualnames in one request",
        );
    }
    // A PRESENT-but-blank hint is a malformed request, rejected with a message
    // that NAMES the field (the proposal's cross-version diagnosability note;
    // adjudicated under clarion-b1a158f7f5). Omitting the field is the legal
    // "no hint" spelling. Any non-blank value is honored as a constraint —
    // an unknown plugin id resolves nothing rather than erroring.
    if let Some(plugin) = &req.plugin
        && plugin.trim().is_empty()
    {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "plugin must not be blank when present (omit the plugin field for cross-plugin resolution)",
        );
    }
    // Move only `qualnames` + the hint into the reader closure; `project` was
    // consumed by the guard above. `with_reader` runs the lookup on a pooled
    // connection.
    let qualnames = req.qualnames;
    let plugin = req.plugin;
    let result = state
        .readers
        .with_reader(move |conn| {
            loomweave_storage::resolve_wardline_qualnames_for_plugin(
                conn,
                &qualnames,
                plugin.as_deref(),
            )
        })
        .await;
    match result {
        Ok(pairs) => {
            let mut resolved = std::collections::BTreeMap::new();
            let mut unresolved = Vec::new();
            for (qualname, resolution) in pairs {
                match resolution.into_entity_id() {
                    Some(id) => {
                        resolved.insert(qualname, id);
                    }
                    None => unresolved.push(qualname),
                }
            }
            (
                StatusCode::OK,
                Json(ResolveResponse {
                    resolved,
                    unresolved,
                }),
            )
                .into_response()
        }
        Err(err) => json_read_error(&err),
    }
}

/// Wardline taint-fact batch WRITE (ADR-036, W.2). Disabled by default; only
/// reachable when `serve.http.wardline_taint_write` spawned the optional
/// writer-actor (`state.taint_writer` is `Some`). Resolution is the SAME
/// exact-tier oracle the resolve endpoint uses; `wardline_json` is opaque and
/// stored verbatim. Facts whose qualname does not resolve are reported in
/// `unresolved_qualnames` and silently skipped (not an error).
pub(crate) async fn post_wardline_taint_facts(
    State(state): State<AppState>,
    body: Result<Json<WriteTaintFactsRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    // Disabled-by-default guard fires BEFORE body parsing: a `None` writer
    // means the API is off regardless of payload shape.
    let Some(writer) = state.taint_writer.clone() else {
        return json_error(
            StatusCode::FORBIDDEN,
            ErrorCode::WriteDisabled,
            "taint-fact write API is disabled (set serve.http.wardline_taint_write: true)",
        );
    };
    let Json(req) = match body {
        Ok(json) => json,
        Err(rej) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidPath,
                &rej.body_text(),
            );
        }
    };
    if let Some(resp) = state.reject_project_mismatch(&req.project) {
        return resp;
    }
    if req.facts.len() > WARDLINE_TAINT_BATCH_MAX {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::BatchTooLarge,
            "too many facts in one request",
        );
    }

    // Resolve every qualname EXACT-only on the reader pool, in input order, in
    // one pooled-connection checkout. Zip results back onto the facts by index
    // (NOT a qualname->id map) so duplicate qualnames are handled correctly. In
    // the SAME checkout, batch-resolve the alive SEI for every resolved locator
    // (one chunked `IN`, not an N+1 of point lookups) so each fact can be
    // stamped with its rename-stable SEI key (T3.4).
    let qualnames: Vec<String> = req.facts.iter().map(|f| f.qualname.clone()).collect();
    let resolution = state
        .readers
        .with_reader(move |conn| {
            let resolved = loomweave_storage::resolve_wardline_qualnames(conn, &qualnames)?;
            let locators: Vec<String> = resolved
                .iter()
                .filter_map(|(_, r)| r.entity_id().map(str::to_owned))
                .collect();
            let seis = loomweave_storage::seis_for_locators(conn, &locators)?;
            Ok::<_, loomweave_storage::StorageError>((resolved, seis))
        })
        .await;
    let (resolved, seis_by_locator) = match resolution {
        Ok(pair) => pair,
        Err(err) => return json_read_error(&err),
    };

    let batch_scan_id = req.scan_id.clone();
    let updated_at = iso8601_now();
    let mut written = 0_usize;
    let mut unresolved_qualnames = Vec::new();
    for (fact, (_, res)) in req.facts.into_iter().zip(resolved) {
        let Some(entity_id) = res.into_entity_id() else {
            unresolved_qualnames.push(fact.qualname);
            continue;
        };
        // SEI key: the server-resolved alive binding is authoritative for the
        // resolved locator. A caller-supplied value is accepted only when it
        // matches that binding; otherwise it would let a client write facts for
        // locator A under locator B's stable identity.
        let resolved_sei = seis_by_locator.get(&entity_id).cloned();
        if let (Some(caller_sei), Some(resolved_sei)) = (&fact.sei, &resolved_sei)
            && caller_sei != resolved_sei
        {
            return json_error(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidPath,
                "caller-supplied sei conflicts with server-resolved locator identity",
            );
        }
        let sei = fact.sei.clone().or(resolved_sei);
        let taint_fact = loomweave_storage::TaintFact {
            entity_id,
            // Opaque + byte-verbatim: `RawValue::get()` returns the original
            // bytes of the blob exactly as the client sent them (no key
            // reordering). Do NOT parse out scan_id/content_hash from inside
            // the blob; do NOT validate it.
            wardline_json: fact.wardline_json.get().to_owned(),
            scan_id: fact.scan_id.or_else(|| batch_scan_id.clone()),
            content_hash_at_compute: fact.content_hash_at_compute.clone(),
            updated_at: updated_at.clone(),
            sei,
        };
        let (ack_tx, ack_rx) = oneshot::channel();
        let cmd = loomweave_storage::WriterCmd::UpsertWardlineTaintFact {
            fact: Box::new(taint_fact),
            ack: ack_tx,
        };
        if writer.send(cmd).await.is_err() {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::StorageError,
                "taint-fact writer is unavailable",
            );
        }
        match ack_rx.await {
            Ok(Ok(())) => written += 1,
            Ok(Err(err)) => {
                log_taint_write_error(&err);
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorCode::Internal,
                    "taint-fact write failed",
                );
            }
            Err(_) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    ErrorCode::Internal,
                    "taint-fact writer dropped the response channel",
                );
            }
        }
    }

    (
        StatusCode::OK,
        Json(WriteTaintFactsResponse {
            written,
            unresolved_qualnames,
        }),
    )
        .into_response()
}

/// Shared read builder for the GET and `:batch-get` taint-fact endpoints.
/// Runs ALL DB work and the live file hashing inside ONE pooled-connection
/// checkout (the `with_reader` closure is the blocking context). For each
/// qualname:
///
/// - resolve exact-tier → entity id (unresolved → `exists: false`);
/// - `get_taint_facts` for the resolved ids; map back by entity id;
/// - for rows that exist, parse the stored blob byte-faithfully via
///   `RawValue::from_string` (W.2 wrote it from a `RawValue`, so it
///   round-trips) and derive `current_content_hash` live from the row's
///   `source_file_path` via `loomweave_storage::current_file_hash`.
///
/// File hashing is DEDUPED per request by `source_file_path`: a chain-walk
/// batch hits many functions sharing one file, and a 425k-LOC project must
/// not re-hash the same file N times. A deleted/renamed/unreadable file →
/// `current_content_hash: None` (a stale signal, not a 500).
///
/// Returns `Err(Response)` only when the DB read itself fails; per-qualname
/// "not found" is conveyed in-band via `exists: false`.
pub(crate) async fn respond_taint_facts(
    state: &AppState,
    qualnames: Vec<String>,
) -> Result<Vec<TaintFactView>, Response> {
    let project_root = state.project_root.clone();
    let result = state
        .readers
        .with_reader(move |conn| {
            // 1. Resolve every qualname (exact tier), in input order.
            let resolved = loomweave_storage::resolve_wardline_qualnames(conn, &qualnames)?;

            // 2. Fetch facts for the resolved entity ids; map back by id.
            let entity_ids: Vec<String> = resolved
                .iter()
                .filter_map(|(_, r)| r.entity_id().map(str::to_owned))
                .collect();
            let rows = loomweave_storage::get_taint_facts(conn, &entity_ids)?;
            let by_entity: std::collections::HashMap<String, loomweave_storage::TaintFactRow> =
                rows.into_iter()
                    .map(|row| (row.entity_id.clone(), row))
                    .collect();

            // 3. Build a view per qualname, deduping file hashing by path.
            let mut file_hash_cache: std::collections::HashMap<String, Option<String>> =
                std::collections::HashMap::new();
            let mut views = Vec::with_capacity(resolved.len());
            for (qualname, resolution) in resolved {
                let view = match resolution
                    .into_entity_id()
                    .and_then(|id| by_entity.get(&id))
                {
                    Some(row) => {
                        // Byte-faithful: the stored string is exactly what W.2
                        // wrote from a RawValue, so it re-parses. A parse error
                        // is a storage-integrity failure, not a 404.
                        let wardline_json =
                            serde_json::value::RawValue::from_string(row.wardline_json.clone())
                                .map_err(|e| {
                                    StorageError::Corruption(format!(
                                        "stored wardline_json for {} is not valid JSON: {e}",
                                        row.entity_id
                                    ))
                                })?;
                        let current_content_hash = match &row.source_file_path {
                            Some(path) => file_hash_cache
                                .entry(path.clone())
                                .or_insert_with(|| {
                                    loomweave_storage::current_file_hash(&project_root, path)
                                })
                                .clone(),
                            None => None,
                        };
                        TaintFactView {
                            qualname,
                            wardline_json: Some(wardline_json),
                            current_content_hash,
                            exists: true,
                        }
                    }
                    // Unresolved qualname OR resolved-but-no-stored-fact.
                    _ => TaintFactView {
                        qualname,
                        wardline_json: None,
                        current_content_hash: None,
                        exists: false,
                    },
                };
                views.push(view);
            }
            Ok(views)
        })
        .await;
    result.map_err(|err| json_read_error(&err))
}

/// Single taint-fact READ (ADR-036, W.3). Reads only — served regardless of
/// `state.taint_writer` (the write API may be disabled). Unknown qualname →
/// `exists: false` with no `wardline_json`.
pub(crate) async fn get_wardline_taint_fact(
    State(state): State<AppState>,
    query: Result<Query<TaintFactQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "query parameters are invalid",
        );
    };
    if let Some(resp) = state.reject_project_mismatch(&query.project) {
        return resp;
    }
    if query.qualname.trim().is_empty() {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "qualname query parameter must not be blank",
        );
    }
    match respond_taint_facts(&state, vec![query.qualname]).await {
        Ok(mut views) => {
            // Exactly one input qualname → exactly one view.
            let view = views.pop().unwrap_or(TaintFactView {
                qualname: String::new(),
                wardline_json: None,
                current_content_hash: None,
                exists: false,
            });
            (StatusCode::OK, Json(view)).into_response()
        }
        Err(resp) => resp,
    }
}

/// Batch taint-fact READ (ADR-036, W.3). One DB checkout + per-request file
/// hash dedup serves the chain-walk batch. Reads only — served regardless of
/// `state.taint_writer`.
pub(crate) async fn post_wardline_taint_facts_batch_get(
    State(state): State<AppState>,
    body: Result<Json<BatchGetRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(json) => json,
        Err(rej) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidPath,
                &rej.body_text(),
            );
        }
    };
    if let Some(resp) = state.reject_project_mismatch(&req.project) {
        return resp;
    }
    if req.qualnames.len() > WARDLINE_TAINT_BATCH_MAX {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::BatchTooLarge,
            "too many qualnames in one request",
        );
    }
    match respond_taint_facts(&state, req.qualnames).await {
        Ok(views) => (StatusCode::OK, Json(views)).into_response(),
        Err(resp) => resp,
    }
}

/// Shared read builder for the read-by-SEI taint-fact endpoint (T3.4).
/// Mirrors [`respond_taint_facts`] but keys on the opaque SEI: for each SEI,
/// fetch the most-recent SEI-tagged fact (regardless of the locator it was
/// written under), parse the stored blob byte-faithfully, and derive the live
/// whole-file `current_content_hash` from the fact's `source_file_path`. File
/// hashing is DEDUPED per request by path. All DB work + hashing run in ONE
/// pooled-connection checkout. A SEI with no stored fact → `exists: false`.
pub(crate) async fn respond_taint_facts_by_sei(
    state: &AppState,
    seis: Vec<String>,
) -> Result<Vec<TaintFactBySeiView>, Response> {
    let project_root = state.project_root.clone();
    let result = state
        .readers
        .with_reader(move |conn| {
            // Most-recent fact per SEI (rename-stable lookup).
            let rows = loomweave_storage::get_taint_facts_by_sei(conn, &seis)?;
            let by_sei: std::collections::HashMap<String, loomweave_storage::TaintFactRow> = rows
                .into_iter()
                .filter_map(|row| row.sei.clone().map(|sei| (sei, row)))
                .collect();

            let mut file_hash_cache: std::collections::HashMap<String, Option<String>> =
                std::collections::HashMap::new();
            let mut views = Vec::with_capacity(seis.len());
            // Emit one view per input SEI, in input order. A duplicate input
            // SEI yields a duplicate view (input shape is the client's).
            for sei in seis {
                let view = match by_sei.get(&sei) {
                    Some(row) => {
                        // Byte-faithful re-parse: the stored string is exactly
                        // what the write path persisted from a RawValue. A parse
                        // error is a storage-integrity failure, not a 404.
                        let wardline_json =
                            serde_json::value::RawValue::from_string(row.wardline_json.clone())
                                .map_err(|e| {
                                    StorageError::Corruption(format!(
                                        "stored wardline_json for {} is not valid JSON: {e}",
                                        row.entity_id
                                    ))
                                })?;
                        let current_content_hash = match &row.source_file_path {
                            Some(path) => file_hash_cache
                                .entry(path.clone())
                                .or_insert_with(|| {
                                    loomweave_storage::current_file_hash(&project_root, path)
                                })
                                .clone(),
                            None => None,
                        };
                        TaintFactBySeiView {
                            sei,
                            wardline_json: Some(wardline_json),
                            current_content_hash,
                            exists: true,
                        }
                    }
                    None => TaintFactBySeiView {
                        sei,
                        wardline_json: None,
                        current_content_hash: None,
                        exists: false,
                    },
                };
                views.push(view);
            }
            Ok(views)
        })
        .await;
    result.map_err(|err| json_read_error(&err))
}

/// Read taint facts by SEI (T3.4). The rename-survival surface: a fact written
/// under a former locator is retrievable by its stable SEI after a rename.
/// Reads only — served regardless of `state.taint_writer`. Opaque inputs: no
/// locator-shape validation; a blank SEI simply matches no row (echoed back as
/// `exists: false`, like any unknown SEI).
pub(crate) async fn post_wardline_taint_facts_batch_get_by_sei(
    State(state): State<AppState>,
    body: Result<Json<BatchGetBySeiRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(req) = match body {
        Ok(json) => json,
        Err(rej) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                ErrorCode::InvalidPath,
                &rej.body_text(),
            );
        }
    };
    if let Some(resp) = state.reject_project_mismatch(&req.project) {
        return resp;
    }
    if req.seis.len() > WARDLINE_TAINT_BATCH_MAX {
        return json_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::BatchTooLarge,
            "too many seis in one request",
        );
    }
    match respond_taint_facts_by_sei(&state, req.seis).await {
        Ok(views) => (StatusCode::OK, Json(views)).into_response(),
        Err(resp) => resp,
    }
}

pub(crate) fn log_taint_write_error(err: &StorageError) {
    let error_chain = format_error_chain(err);
    tracing::dispatcher::with_default(&HTTP_ERROR_DISPATCH, || {
        tracing::error!(
            error_chain = %error_chain,
            "HTTP /api/wardline/taint-facts write failed"
        );
    });
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::header;

    use super::*;
    use crate::http_read::test_support::{hmac_request, json_body};
    use crate::http_read::{HTTP_BODY_LIMIT_BYTES, WARDLINE_BODY_LIMIT_BYTES, router};
    use std::sync::Arc;

    /// Build an `AppState` over a fresh temp file DB with migrations applied
    /// and the given entity ids seeded as full `entities` rows. Returns the
    /// state plus the `TempDir` guard (drop it last). The state carries an
    /// HMAC `identity_secret` so the protected/wardline routes are exercised
    /// with real signature verification.
    fn wardline_resolve_test_state(
        secret: &str,
        seed_ids: &[&str],
    ) -> (AppState, tempfile::TempDir) {
        use loomweave_storage::ReaderPool;
        use loomweave_storage::schema::apply_migrations;

        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("loomweave.db");
        let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
        apply_migrations(&mut conn).expect("apply migrations");
        for id in seed_ids {
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, created_at, updated_at \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id,
                    "python",
                    "function",
                    id,
                    id.rsplit('.').next().unwrap_or(id),
                    "{}",
                    "deadbeef",
                    "2026-05-31T00:00:00.000Z",
                    "2026-05-31T00:00:00.000Z",
                ],
            )
            .expect("seed entity row");
        }
        drop(conn);

        let readers = ReaderPool::open(&db_path, 4).expect("open reader pool");
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000004")
                .expect("parse synthetic instance id");
        let state = AppState {
            project_root: tempdir.path().to_path_buf(),
            readers,
            instance_id,
            auth_token: None,
            identity_secret: Some(Arc::new(secret.to_owned())),
            hmac_replay_cache: crate::http_read::auth::new_hmac_replay_cache(),
            taint_writer: None,
        };
        (state, tempdir)
    }

    #[tokio::test]
    async fn wardline_resolve_returns_exact_matches_and_unresolved() {
        use tower::ServiceExt;

        let secret = "wardline-resolve-secret";
        let (state, _tempdir) = wardline_resolve_test_state(secret, &["python:function:a.b.c"]);
        let body = br#"{"qualnames":["a.b.c","x.y.z"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/resolve", body);

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parsed["resolved"]["a.b.c"], "python:function:a.b.c");
        assert_eq!(
            parsed["resolved"]
                .as_object()
                .expect("resolved object")
                .len(),
            1,
            "only exact matches appear in resolved: {parsed}"
        );
        assert_eq!(parsed["unresolved"], serde_json::json!(["x.y.z"]));
    }

    #[tokio::test]
    async fn wardline_resolve_rejects_project_guard_mismatch() {
        use tower::ServiceExt;

        let secret = "wardline-resolve-secret";
        let (state, _tempdir) = wardline_resolve_test_state(secret, &[]);
        // A non-empty `project` that does not match the served project-root
        // directory name must be rejected with 403 PROJECT_MISMATCH.
        let body = br#"{"project":"some-other-project","qualnames":["a.b.c"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/resolve", body);

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = to_bytes(response.into_body(), 4096)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "PROJECT_MISMATCH");
    }

    /// Insert one `function` entity under an explicit `plugin_id` into an
    /// existing migrated DB. The ambiguity tests need a `rust:function:` row
    /// alongside a `python:function:` one so per-plugin candidate minting
    /// (clarion-69db8b2739) sees the same qualname under both plugins.
    fn insert_function_entity(db_path: &std::path::Path, plugin: &str, id: &str) {
        let conn = rusqlite::Connection::open(db_path).expect("open db");
        conn.execute(
            "INSERT INTO entities ( \
                id, plugin_id, kind, name, short_name, properties, \
                content_hash, created_at, updated_at \
             ) VALUES (?1, ?2, 'function', ?1, ?1, '{}', 'deadbeef', \
                       '2026-05-31T00:00:00.000Z', '2026-05-31T00:00:00.000Z')",
            rusqlite::params![id, plugin],
        )
        .expect("seed function entity");
    }

    /// Insert a `wardline_taint_facts` row verbatim into an existing migrated
    /// DB (for read tests over a non-python locator, where the python-seeding
    /// state builders cannot place the fact).
    fn insert_taint_fact(db_path: &std::path::Path, entity_id: &str, blob: &str) {
        let conn = rusqlite::Connection::open(db_path).expect("open db");
        conn.execute(
            "INSERT INTO wardline_taint_facts \
                (entity_id, wardline_json, scan_id, content_hash_at_compute, updated_at) \
             VALUES (?1, ?2, NULL, NULL, '2026-05-31T00:00:00.000Z')",
            rusqlite::params![entity_id, blob],
        )
        .expect("seed taint fact");
    }

    #[tokio::test]
    async fn wardline_resolve_returns_rust_only_qualname_as_resolved() {
        // Positive rust coverage on the federation resolve route
        // (clarion-69db8b2739): a qualname that exists ONLY under the `rust`
        // plugin resolves Exact and appears in the resolved map with its
        // `rust:function:` id — the route is no longer python-only.
        use tower::ServiceExt;

        let secret = "wardline-resolve-secret";
        let (state, tempdir) = wardline_resolve_test_state(secret, &[]);
        let db_path = tempdir.path().join("loomweave.db");
        insert_function_entity(&db_path, "rust", "rust:function:mcp_fixture.ops.entry");

        let body = br#"{"qualnames":["mcp_fixture.ops.entry"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/resolve", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(
            parsed["resolved"]["mcp_fixture.ops.entry"], "rust:function:mcp_fixture.ops.entry",
            "rust-only qualname resolves to its rust:function: id: {parsed}"
        );
        assert_eq!(
            parsed["resolved"]
                .as_object()
                .expect("resolved object")
                .len(),
            1
        );
        assert_eq!(
            parsed["unresolved"],
            serde_json::json!([]),
            "nothing unresolved: {parsed}"
        );
    }

    #[tokio::test]
    async fn wardline_taint_write_persists_fact_under_rust_locator() {
        // Positive rust coverage on the WRITE route: a rust-only qualname
        // resolves Exact, so its fact is persisted under the `rust:function:`
        // locator (written count + DB row both confirm it).
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, db_path, writer, _tempdir) = wardline_write_test_state(secret, &[]);
        insert_function_entity(&db_path, "rust", "rust:function:mcp_fixture.ops.entry");

        let blob = r#"{"taint":{"ret":"RAW"},"v":1}"#;
        let body = format!(
            r#"{{"facts":[{{"qualname":"mcp_fixture.ops.entry","wardline_json":{blob}}}]}}"#
        );
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body.as_bytes());
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["written"], 1, "rust-locator fact written: {parsed}");
        assert_eq!(parsed["unresolved_qualnames"], serde_json::json!([]));

        let stored = read_taint_blob(&db_path, "rust:function:mcp_fixture.ops.entry")
            .expect("fact stored under the rust locator");
        assert_eq!(stored, blob, "blob stored byte-verbatim");
        drop(writer);
    }

    #[tokio::test]
    async fn wardline_taint_read_returns_fact_stored_under_rust_locator() {
        // Positive rust coverage on the READ route: a fact stored under a
        // `rust:function:` locator is retrievable by its bare qualname.
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let (state, tempdir) = wardline_read_test_state(secret, &[]);
        let db_path = tempdir.path().join("loomweave.db");
        insert_function_entity(&db_path, "rust", "rust:function:mcp_fixture.ops.entry");
        let blob = r#"{"rust":true,"v":1}"#;
        insert_taint_fact(&db_path, "rust:function:mcp_fixture.ops.entry", blob);

        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=mcp_fixture.ops.entry",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let text = String::from_utf8(body.to_vec()).expect("utf8");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("json");

        assert_eq!(parsed["qualname"], "mcp_fixture.ops.entry");
        assert_eq!(parsed["exists"], true, "rust-locator fact found: {parsed}");
        assert!(
            text.contains(r#""wardline_json":{"rust":true,"v":1}"#),
            "wardline_json byte-faithful: {text}"
        );
        // The seeded rust entity row carries no source_file_path, so the live
        // freshness hash degrades to null (a stale signal, not an error).
        assert_eq!(parsed["current_content_hash"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn wardline_taint_read_fact_eclipsed_when_qualname_turns_ambiguous() {
        // Fact eclipse (read-route ambiguous degradation): a fact persisted
        // while `dual.target` was Exact under `python` becomes unreachable BY
        // QUALNAME once a same-qualname `rust` entity appears — resolution
        // flips Exact→Ambiguous, the accessors degrade it to unresolved, and
        // the read routes report `exists: false`. The stored row is NOT
        // deleted: it remains reachable by SEI/locator, only the qualname
        // lookup is eclipsed (ADR-036 Amendment 2026-06-11).
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, db_path, writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:dual.target"]);

        // 1. Write the fact while the qualname is unambiguous (Exact → written).
        let body = br#"{"facts":[{"qualname":"dual.target","wardline_json":{"v":1}}]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);
        let response = router(state.clone())
            .oneshot(request)
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["written"], 1, "fact written while Exact: {parsed}");
        assert!(
            read_taint_blob(&db_path, "python:function:dual.target").is_some(),
            "fact persisted under the python locator"
        );

        // 2. A same-qualname rust entity appears (e.g. a later analyze run).
        insert_function_entity(&db_path, "rust", "rust:function:dual.target");

        // 3. Single GET: the qualname now resolves Ambiguous → exists: false.
        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=dual.target",
            b"",
        );
        let response = router(state.clone())
            .oneshot(request)
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            parsed["exists"], false,
            "qualname-eclipsed fact must read as exists: false: {parsed}"
        );
        assert!(
            parsed.get("wardline_json").is_none(),
            "eclipsed fact must not leak its blob: {parsed}"
        );

        // 4. Batch-get degrades identically.
        let body = br#"{"qualnames":["dual.target"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts:batch-get", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["qualname"], "dual.target");
        assert_eq!(arr[0]["exists"], false, "batch-get eclipsed too: {parsed}");

        // 5. Eclipsed, not erased: the stored row survives untouched.
        assert_eq!(
            read_taint_blob(&db_path, "python:function:dual.target").as_deref(),
            Some(r#"{"v":1}"#),
            "the fact row still exists in the DB (qualname-eclipsed, not deleted)"
        );
        drop(writer);
    }

    // ── ResolveRequest plugin hint (clarion-b1a158f7f5, ADR-036 amendment) ──
    // Wire-level pins for the agreed Wardline proposal shape
    // (wardline/docs/integration/2026-06-11-wardline-resolve-plugin-hint-proposal.md),
    // including the proposal's three conformance cases: hinted-hit,
    // hinted-miss, and unhinted-ambiguous (see
    // wardline_resolve_plugin_hint_disambiguates_dual_qualname).

    /// Helper: POST /api/wardline/resolve with the given body, expect 200 and
    /// return the parsed JSON response.
    async fn resolve_ok(state: AppState, secret: &str, body: &[u8]) -> serde_json::Value {
        use tower::ServiceExt;
        let request = hmac_request(secret, "POST", "/api/wardline/resolve", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn wardline_resolve_plugin_hint_hit_resolves_rust_only_qualname() {
        // hinted-hit: rust hint + rust-only qualname → resolved with the
        // rust:function: id.
        let secret = "wardline-resolve-secret";
        let (state, tempdir) = wardline_resolve_test_state(secret, &[]);
        let db_path = tempdir.path().join("loomweave.db");
        insert_function_entity(&db_path, "rust", "rust:function:mcp_fixture.ops.entry");

        let body = br#"{"qualnames":["mcp_fixture.ops.entry"],"plugin":"rust"}"#;
        let parsed = resolve_ok(state, secret, body).await;
        assert_eq!(
            parsed["resolved"]["mcp_fixture.ops.entry"], "rust:function:mcp_fixture.ops.entry",
            "hinted-hit resolves to the rust id: {parsed}"
        );
        assert_eq!(parsed["unresolved"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn wardline_resolve_plugin_hint_miss_other_plugin_owned_is_unresolved() {
        // hinted-miss: the qualname is owned ONLY by python; a rust hint must
        // return it unresolved. The hint is a CONSTRAINT, never a preference
        // order that falls back to whoever owns the qualname.
        let secret = "wardline-resolve-secret";
        let (state, _tempdir) =
            wardline_resolve_test_state(secret, &["python:function:py.only.fn"]);

        let body = br#"{"qualnames":["py.only.fn"],"plugin":"rust"}"#;
        let parsed = resolve_ok(state, secret, body).await;
        assert!(
            parsed["resolved"]
                .as_object()
                .expect("resolved object")
                .is_empty(),
            "a hinted miss must NOT fall back to the other plugin: {parsed}"
        );
        assert_eq!(parsed["unresolved"], serde_json::json!(["py.only.fn"]));
    }

    #[tokio::test]
    async fn wardline_resolve_plugin_hint_disambiguates_dual_qualname() {
        // The headline case: a qualname under BOTH plugins is unresolved
        // unhinted (Ambiguous degrades on the wire), but a rust hint pins it
        // to the rust id — on the SAME store state.
        let secret = "wardline-resolve-secret";
        let (state, tempdir) =
            wardline_resolve_test_state(secret, &["python:function:dual.target"]);
        let db_path = tempdir.path().join("loomweave.db");
        insert_function_entity(&db_path, "rust", "rust:function:dual.target");

        // Unhinted on this state: still unresolved (the pre-hint behavior).
        let parsed = resolve_ok(state.clone(), secret, br#"{"qualnames":["dual.target"]}"#).await;
        assert_eq!(
            parsed["unresolved"],
            serde_json::json!(["dual.target"]),
            "unhinted dual-plugin qualname stays unresolved: {parsed}"
        );

        // Hinted: the rust hint disambiguates to the rust id.
        let parsed = resolve_ok(
            state,
            secret,
            br#"{"qualnames":["dual.target"],"plugin":"rust"}"#,
        )
        .await;
        assert_eq!(
            parsed["resolved"]["dual.target"], "rust:function:dual.target",
            "the rust hint disambiguates the dual qualname: {parsed}"
        );
        assert_eq!(parsed["unresolved"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn wardline_resolve_plugin_hint_python_symmetric_hit() {
        // Symmetric python hint on the dual-plugin state resolves the python id.
        let secret = "wardline-resolve-secret";
        let (state, tempdir) =
            wardline_resolve_test_state(secret, &["python:function:dual.target"]);
        let db_path = tempdir.path().join("loomweave.db");
        insert_function_entity(&db_path, "rust", "rust:function:dual.target");

        let body = br#"{"qualnames":["dual.target"],"plugin":"python"}"#;
        let parsed = resolve_ok(state, secret, body).await;
        assert_eq!(
            parsed["resolved"]["dual.target"], "python:function:dual.target",
            "the python hint resolves the python id: {parsed}"
        );
        assert_eq!(parsed["unresolved"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn wardline_resolve_blank_plugin_is_400_naming_the_field() {
        // Adjudicated under clarion-b1a158f7f5: a blank/whitespace-only
        // `plugin` is a 400 whose message NAMES the field (cross-version
        // diagnosability per the proposal's rollout note) — not a silent
        // whole-batch unresolved.
        use tower::ServiceExt;

        let secret = "wardline-resolve-secret";
        for body in [
            br#"{"qualnames":["a.b.c"],"plugin":""}"#.as_slice(),
            br#"{"qualnames":["a.b.c"],"plugin":"  "}"#.as_slice(),
        ] {
            let (state, _tempdir) = wardline_resolve_test_state(secret, &["python:function:a.b.c"]);
            let request = hmac_request(secret, "POST", "/api/wardline/resolve", body);
            let response = router(state).oneshot(request).await.expect("oneshot");
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let bytes = to_bytes(response.into_body(), 4096)
                .await
                .expect("read body");
            let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
            let message = parsed["error"].as_str().expect("error message");
            assert!(
                message.contains("plugin"),
                "the 400 must NAME the plugin field: {parsed}"
            );
            assert!(
                message.contains("blank"),
                "the 400 must say WHY (blank), not just reject: {parsed}"
            );
        }
    }

    #[tokio::test]
    async fn wardline_resolve_unknown_plugin_returns_all_unresolved() {
        // A non-blank unknown plugin id is a constraint nothing satisfies:
        // 200 with everything unresolved — NOT a 400 (plugin ids are not
        // validated against the store; adjudicated under clarion-b1a158f7f5).
        let secret = "wardline-resolve-secret";
        let (state, _tempdir) = wardline_resolve_test_state(secret, &["python:function:a.b.c"]);

        let body = br#"{"qualnames":["a.b.c"],"plugin":"java"}"#;
        let parsed = resolve_ok(state, secret, body).await;
        assert!(
            parsed["resolved"]
                .as_object()
                .expect("resolved object")
                .is_empty(),
            "an unknown plugin constraint resolves nothing: {parsed}"
        );
        assert_eq!(parsed["unresolved"], serde_json::json!(["a.b.c"]));
    }

    #[tokio::test]
    async fn wardline_resolve_degrades_ambiguous_to_unresolved() {
        // clarion-69db8b2739: the same qualname under BOTH plugins resolves
        // Ambiguous; the federation accessor degrades it to "unresolved" (never
        // pick a plugin arbitrarily — ADR-036 exact-only-write), so the qualname
        // appears in `unresolved`, NOT in `resolved`, and the single-id
        // ResolveResponse wire shape is preserved.
        use tower::ServiceExt;

        let secret = "wardline-resolve-secret";
        let (state, tempdir) =
            wardline_resolve_test_state(secret, &["python:function:dual.target"]);
        let db_path = tempdir.path().join("loomweave.db");
        insert_function_entity(&db_path, "rust", "rust:function:dual.target");

        let body = br#"{"qualnames":["dual.target"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/resolve", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");

        assert!(
            parsed["resolved"]
                .as_object()
                .expect("resolved object")
                .is_empty(),
            "an ambiguous qualname must NOT appear in resolved: {parsed}"
        );
        assert_eq!(
            parsed["unresolved"],
            serde_json::json!(["dual.target"]),
            "an ambiguous qualname degrades to unresolved: {parsed}"
        );
    }

    #[tokio::test]
    async fn wardline_taint_write_reports_ambiguous_as_unresolved_and_writes_nothing() {
        // Write-path counterpart: an ambiguous qualname is reported in
        // `unresolved_qualnames` and nothing is persisted under either plugin's
        // locator (exact-only-write preserved).
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, db_path, writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:dual.target"]);
        insert_function_entity(&db_path, "rust", "rust:function:dual.target");

        let body = br#"{"facts":[{"qualname":"dual.target","wardline_json":{"v":1}}]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            parsed["written"], 0,
            "nothing written for ambiguous: {parsed}"
        );
        assert_eq!(
            parsed["unresolved_qualnames"],
            serde_json::json!(["dual.target"])
        );
        assert!(
            read_taint_blob(&db_path, "python:function:dual.target").is_none(),
            "no python-locator fact written"
        );
        assert!(
            read_taint_blob(&db_path, "rust:function:dual.target").is_none(),
            "no rust-locator fact written"
        );
        drop(writer);
    }

    /// Build a write-enabled `AppState` over a fresh temp migrated DB with the
    /// given entity ids seeded, plus a REAL writer-actor. Returns the state, the
    /// `db_path` (for verification on a fresh connection), the `Writer` handle
    /// (drop it last so the actor can flush), and the `TempDir` guard. The
    /// actor runs via `Writer::spawn`'s `spawn_blocking`, so the caller MUST be
    /// on a tokio runtime (`#[tokio::test]`).
    fn wardline_write_test_state(
        secret: &str,
        seed_ids: &[&str],
    ) -> (
        AppState,
        std::path::PathBuf,
        loomweave_storage::Writer,
        tempfile::TempDir,
    ) {
        wardline_write_test_state_with_bindings(secret, seed_ids, &[])
    }

    fn wardline_write_test_state_with_bindings(
        secret: &str,
        seed_ids: &[&str],
        sei_bindings: &[(&str, &str)],
    ) -> (
        AppState,
        std::path::PathBuf,
        loomweave_storage::Writer,
        tempfile::TempDir,
    ) {
        use loomweave_storage::ReaderPool;
        use loomweave_storage::schema::apply_migrations;

        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("loomweave.db");
        let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
        apply_migrations(&mut conn).expect("apply migrations");
        for id in seed_ids {
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, created_at, updated_at \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    id,
                    "python",
                    "function",
                    id,
                    id.rsplit('.').next().unwrap_or(id),
                    "{}",
                    "deadbeef",
                    "2026-05-31T00:00:00.000Z",
                    "2026-05-31T00:00:00.000Z",
                ],
            )
            .expect("seed entity row");
        }
        for (sei, locator) in sei_bindings {
            conn.execute(
                "INSERT INTO sei_bindings \
                    (sei, current_locator, body_hash, signature, status, \
                     born_run_id, updated_run_id, updated_at) \
                 VALUES (?1, ?2, NULL, NULL, 'alive', 'run-0', 'run-0', 't')",
                rusqlite::params![sei, locator],
            )
            .expect("seed SEI binding row");
        }
        drop(conn);

        let readers = ReaderPool::open(&db_path, 4).expect("open reader pool");
        let (writer, _join) = loomweave_storage::Writer::spawn(
            db_path.clone(),
            loomweave_storage::DEFAULT_BATCH_SIZE,
            loomweave_storage::DEFAULT_CHANNEL_CAPACITY,
        )
        .expect("spawn taint writer-actor");
        // The join handle is dropped here: the test reads the DB on a fresh
        // connection AFTER awaiting per-upsert acks, which confirm durability
        // (query_time_write auto-commits before the ack fires).
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000005")
                .expect("parse synthetic instance id");
        let state = AppState {
            project_root: tempdir.path().to_path_buf(),
            readers,
            instance_id,
            auth_token: None,
            identity_secret: Some(Arc::new(secret.to_owned())),
            hmac_replay_cache: crate::http_read::auth::new_hmac_replay_cache(),
            taint_writer: Some(writer.sender()),
        };
        (state, db_path, writer, tempdir)
    }

    fn read_taint_blob(db_path: &std::path::Path, entity_id: &str) -> Option<String> {
        let conn = rusqlite::Connection::open(db_path).expect("open verification conn");
        conn.query_row(
            "SELECT wardline_json FROM wardline_taint_facts WHERE entity_id = ?1",
            rusqlite::params![entity_id],
            |row| row.get::<_, String>(0),
        )
        .ok()
    }

    #[tokio::test]
    async fn wardline_taint_write_disabled_returns_403() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        // `wardline_resolve_test_state` builds a state with `taint_writer: None`.
        let (state, _tempdir) = wardline_resolve_test_state(secret, &[]);
        let body = br#"{"facts":[{"qualname":"a.b.c","wardline_json":{"v":1}}]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = to_bytes(response.into_body(), 4096)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "WRITE_DISABLED");
    }

    #[tokio::test]
    async fn wardline_taint_write_persists_resolved_and_reports_unresolved() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, db_path, writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:a.b.c"]);
        // The resolved blob's keys are in NON-alphabetical order
        // (`b` before `a`, `schema` before `ret`). Under the old
        // `Value::to_string()` path serde's BTreeMap would re-emit them
        // alphabetized; `RawValue` preserves the original bytes exactly.
        let resolved_blob = r#"{"b":2,"a":1,"taint":{"ret":"RAW","schema":"w-1"}}"#;
        let body = format!(
            r#"{{"facts":[
            {{"qualname":"a.b.c","wardline_json":{resolved_blob}}},
            {{"qualname":"x.y.z","wardline_json":{{"v":2}}}}
        ]}}"#
        );
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body.as_bytes());

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["written"], 1);
        assert_eq!(parsed["unresolved_qualnames"], serde_json::json!(["x.y.z"]));

        // The ack we awaited inside the handler confirms durability; the blob
        // must round-trip BYTE-EXACT — key order preserved, NOT alphabetized.
        // This assertion fails under the old `Value::to_string()` path.
        let stored = read_taint_blob(&db_path, "python:function:a.b.c").expect("fact stored");
        assert_eq!(
            stored, resolved_blob,
            "wardline_json stored byte-verbatim (key order preserved)"
        );
        drop(writer);
    }

    #[tokio::test]
    async fn wardline_taint_write_rejects_conflicting_caller_sei() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let locator = "python:function:a.b.c";
        let resolved_sei = "loomweave:eid:resolved";
        let (state, db_path, writer, _tempdir) =
            wardline_write_test_state_with_bindings(secret, &[locator], &[(resolved_sei, locator)]);

        let body = br#"{"facts":[{"qualname":"a.b.c","sei":"loomweave:eid:other","wardline_json":{"v":1}}]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);
        let response = router(state).oneshot(request).await.expect("oneshot");

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let parsed: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(parsed["code"], "INVALID_PATH");
        assert!(
            read_taint_blob(&db_path, locator).is_none(),
            "conflicting SEI write must not persist"
        );
        drop(writer);
    }

    #[tokio::test]
    async fn wardline_taint_write_accepts_matching_caller_sei() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let locator = "python:function:a.b.c";
        let resolved_sei = "loomweave:eid:resolved";
        let (state, db_path, writer, _tempdir) =
            wardline_write_test_state_with_bindings(secret, &[locator], &[(resolved_sei, locator)]);

        let body = br#"{"facts":[{"qualname":"a.b.c","sei":"loomweave:eid:resolved","wardline_json":{"v":1}}]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);
        let response = router(state).oneshot(request).await.expect("oneshot");

        assert_eq!(response.status(), StatusCode::OK);
        let conn = rusqlite::Connection::open(&db_path).expect("open db");
        let stored_sei: String = conn
            .query_row(
                "SELECT sei FROM wardline_taint_facts WHERE entity_id = ?1",
                rusqlite::params![locator],
                |row| row.get(0),
            )
            .expect("stored fact sei");
        assert_eq!(stored_sei, resolved_sei);
        drop(writer);
    }

    #[tokio::test]
    async fn wardline_taint_write_replaces_per_entity() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, db_path, writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:a.b.c"]);

        let send = |body: &'static [u8]| {
            let state = state.clone();
            async move {
                let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);
                let response = router(state).oneshot(request).await.expect("oneshot");
                assert_eq!(response.status(), StatusCode::OK);
            }
        };
        send(br#"{"facts":[{"qualname":"a.b.c","wardline_json":{"v":1}}]}"#).await;
        send(br#"{"facts":[{"qualname":"a.b.c","wardline_json":{"v":2}}]}"#).await;

        let stored = read_taint_blob(&db_path, "python:function:a.b.c").expect("fact stored");
        assert_eq!(
            stored,
            serde_json::json!({"v":2}).to_string(),
            "second write overwrites"
        );
        drop(writer);
    }

    #[tokio::test]
    async fn wardline_taint_write_rejects_project_guard_mismatch() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, _db_path, writer, _tempdir) = wardline_write_test_state(secret, &[]);
        let body = br#"{"project":"some-other-project","facts":[{"qualname":"a.b.c","wardline_json":{"v":1}}]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = to_bytes(response.into_body(), 4096)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "PROJECT_MISMATCH");
        drop(writer);
    }

    #[tokio::test]
    async fn wardline_taint_write_rejects_oversize_batch() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, _db_path, writer, _tempdir) = wardline_write_test_state(secret, &[]);
        let facts: Vec<serde_json::Value> = (0..=WARDLINE_TAINT_BATCH_MAX)
            .map(
                |i| serde_json::json!({ "qualname": format!("pkg.mod.f{i}"), "wardline_json": {} }),
            )
            .collect();
        assert!(facts.len() > WARDLINE_TAINT_BATCH_MAX);
        let body = serde_json::to_vec(&serde_json::json!({ "facts": facts })).expect("json");
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", &body);

        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let bytes = to_bytes(response.into_body(), 4096)
            .await
            .expect("read body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "BATCH_TOO_LARGE");
        drop(writer);
    }

    /// Identity-guard regression lock for the wardline route group. All three
    /// wardline routes share ONE `require_http_identity_wardline` layer, so the
    /// mutating POST is a sufficient witness: if a wiring regression dropped the
    /// `.route_layer(...)`, an absent-header POST would reach the handler and
    /// return 403/200 — never 401. The trio pins:
    ///   - valid signature → clears the guard (403 `WRITE_DISABLED` on the
    ///     write-disabled state is downstream of auth, so it proves the guard
    ///     passed, independent of the write feature);
    ///   - wrong signature → 401 `UNAUTHENTICATED`;
    ///   - absent header → 401 `UNAUTHENTICATED` (the case that catches a dropped
    ///     `.route_layer`).
    #[tokio::test]
    async fn wardline_taint_write_enforces_identity() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let body = br#"{"facts":[{"qualname":"a.b.c","wardline_json":{"v":1}}]}"#;

        // (1) Valid signature clears the guard. Against the write-DISABLED state
        // (taint_writer: None) the handler then returns 403 WRITE_DISABLED,
        // which is downstream of auth — so reaching it proves the guard passed.
        let (state, _td1) = wardline_resolve_test_state(secret, &[]);
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "a valid signature must clear the identity guard (403 is downstream of auth)"
        );

        // (2) Wrong signature → 401 UNAUTHENTICATED.
        let (state, _td2) = wardline_resolve_test_state(secret, &[]);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/wardline/taint-facts")
            .header("X-Weft-Component", "loomweave:deadbeefdeadbeef")
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_vec()))
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "a wrong signature must be rejected with 401"
        );
        let bytes = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "UNAUTHENTICATED");

        // (3) Absent X-Weft-Component header → 401. This is the case that
        // catches a regression dropping the route_layer: with no guard, this
        // request would reach the handler and 403/200, not 401.
        let (state, _td3) = wardline_resolve_test_state(secret, &[]);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/wardline/taint-facts")
            .header(header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(body.to_vec()))
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "an absent identity header must 401 — dropping the route_layer fails here"
        );
        let bytes = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "UNAUTHENTICATED");
    }

    /// Finding 3 (non-atomic batch): pins the invariant that makes partial
    /// persistence acceptable — a whole-batch re-post is idempotent. Posting a
    /// MULTI-fact batch twice converges to the same state: stable `written`, no
    /// row duplication, last-write-wins per entity. (Deterministic mid-batch
    /// fault injection has no seam in the writer-actor without a test-only hook
    /// in production code; idempotency is the contract-relevant invariant, and
    /// is exactly what `contracts.md` instructs clients to rely on after a 5xx.)
    #[tokio::test]
    async fn wardline_taint_write_batch_retry_is_idempotent() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, db_path, writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:a.b.c", "python:function:d.e.f"]);

        let body = br#"{"facts":[
            {"qualname":"a.b.c","wardline_json":{"v":1}},
            {"qualname":"d.e.f","wardline_json":{"v":2}}
        ]}"#;
        let post = |body: &'static [u8]| {
            let state = state.clone();
            async move {
                let request = hmac_request(secret, "POST", "/api/wardline/taint-facts", body);
                let response = router(state).oneshot(request).await.expect("oneshot");
                assert_eq!(response.status(), StatusCode::OK);
                let bytes = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
                    .await
                    .expect("body");
                serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")
            }
        };

        let first = post(body).await;
        assert_eq!(first["written"], 2);
        let second = post(body).await;
        assert_eq!(
            second["written"], 2,
            "a whole-batch re-post writes the same count"
        );

        // No duplication: exactly one row per entity, last-write-wins.
        assert_eq!(
            read_taint_blob(&db_path, "python:function:a.b.c").as_deref(),
            Some(r#"{"v":1}"#)
        );
        assert_eq!(
            read_taint_blob(&db_path, "python:function:d.e.f").as_deref(),
            Some(r#"{"v":2}"#)
        );
        let count = {
            let conn = rusqlite::Connection::open(&db_path).expect("verify conn");
            conn.query_row("SELECT COUNT(*) FROM wardline_taint_facts", [], |r| {
                r.get::<_, i64>(0)
            })
            .expect("count")
        };
        assert_eq!(count, 2, "re-post must not duplicate rows");
        drop(writer);
    }

    /// The Wardline body-limit relocation is load-bearing: a >16 KiB body must
    /// be accepted on `/api/wardline/resolve` (4 MiB limit) while the SAME body
    /// is still 413'd on the 16 KiB `/api/v1/files/batch` route. A small body
    /// passes everywhere and would not catch a broken relocation.
    #[tokio::test]
    async fn wardline_resolve_accepts_large_body_but_files_batch_rejects_it() {
        use tower::ServiceExt;

        let secret = "wardline-resolve-secret";

        // Build a >16 KiB JSON body of qualnames (well under the 2000 batch
        // cap and under 4 MiB). Each entry is ~30 bytes; 2000 of them clears
        // 16 KiB comfortably.
        let qualnames: Vec<String> = (0..2000).map(|i| format!("pkg.mod.func_{i:05}")).collect();
        let wardline_body =
            serde_json::to_vec(&serde_json::json!({ "qualnames": qualnames })).expect("json");
        assert!(
            wardline_body.len() > HTTP_BODY_LIMIT_BYTES,
            "test body must exceed the 16 KiB limit to be discriminating: {}",
            wardline_body.len()
        );

        let (state, _tempdir) = wardline_resolve_test_state(secret, &[]);
        let request = hmac_request(secret, "POST", "/api/wardline/resolve", &wardline_body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "wardline route must accept a >16 KiB body under its 4 MiB limit"
        );

        // Same-sized body shaped for the files/batch route — must be rejected
        // by the 16 KiB limit (413 from the framework's RequestBodyLimitLayer).
        let batch_body = serde_json::to_vec(&serde_json::json!({ "queries": [] })).expect("json");
        // Pad with a large dummy so the body exceeds 16 KiB but is otherwise
        // a structurally-irrelevant oversize; the limit fires before parsing.
        let mut oversize = batch_body;
        oversize.resize(HTTP_BODY_LIMIT_BYTES + 1024, b' ');
        let (state2, _tempdir2) = wardline_resolve_test_state(secret, &[]);
        let request2 = hmac_request(secret, "POST", "/api/v1/files/batch", &oversize);
        let response2 = router(state2).oneshot(request2).await.expect("oneshot");
        // In HMAC mode the v1 route has TWO oversize-body rejecters: the
        // `RequestBodyLimitLayer(16 KiB)` on the v1 group (→ 413) and the HMAC
        // middleware's own `to_bytes(_, HTTP_BODY_LIMIT_BYTES)` (→ 500). The
        // HMAC read fires first, so this half only proves the SAME body the
        // wardline route accepted is NOT accepted here — it does NOT prove the
        // v1 `RequestBodyLimitLayer` is wired. The no-auth assertion below
        // closes that gap.
        assert_ne!(
            response2.status(),
            StatusCode::OK,
            "files/batch route must reject a >16 KiB body that the wardline route accepts"
        );
        assert!(
            response2.status().is_client_error() || response2.status().is_server_error(),
            "files/batch >16 KiB body must be an error status, got {}",
            response2.status()
        );

        // Regression guard for the v1 `RequestBodyLimitLayer` itself. With NO
        // identity configured (loopback trust), the auth middleware is a plain
        // passthrough and never reads the body, so the ONLY thing that can cap
        // an oversize v1 body is the group's `RequestBodyLimitLayer(16 KiB)`.
        // If that layer were removed in a future refactor, this assertion would
        // flip from 413 to 200 (oversize read silently let through) — which the
        // HMAC-mode half above cannot detect.
        let (mut state3, _tempdir3) = wardline_resolve_test_state(secret, &[]);
        state3.identity_secret = None;
        state3.auth_token = None;
        let request3 = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/files/batch")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_LENGTH, oversize.len().to_string())
            .body(axum::body::Body::from(oversize.clone()))
            .expect("build request");
        let response3 = router(state3).oneshot(request3).await.expect("oneshot");
        assert_eq!(
            response3.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "v1 RequestBodyLimitLayer must 413 a >16 KiB body on the no-auth path"
        );
    }

    /// A seeded function entity for a read test: its qualname, the absolute
    /// path of its containing file (written with `bytes`), and the stored taint
    /// blob (verbatim). `line_start`/`line_end` bound a span inside the file so
    /// the span-vs-whole-file distinction is observable.
    struct SeedFn {
        qualname: &'static str,
        bytes: &'static [u8],
        /// `Some(json)` stores a taint fact; `None` seeds the entity ONLY (the
        /// resolved-entity-but-no-stored-fact case the read path must report
        /// as `exists: false`).
        blob: Option<&'static str>,
    }

    /// Build a reads-only `AppState` (`taint_writer: None`) over a fresh temp
    /// migrated DB. Each `SeedFn` gets a real file written under the project
    /// root and an `entities` row whose `source_file_path` is that file's
    /// ABSOLUTE path; a `wardline_taint_facts` row carrying its blob verbatim
    /// is stored only when `blob` is `Some`. Returns the state and the
    /// `TempDir` guard (drop it last).
    fn wardline_read_test_state(secret: &str, seeds: &[SeedFn]) -> (AppState, tempfile::TempDir) {
        use loomweave_storage::ReaderPool;
        use loomweave_storage::schema::apply_migrations;

        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("loomweave.db");
        let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
        apply_migrations(&mut conn).expect("apply migrations");

        for (i, seed) in seeds.iter().enumerate() {
            let file = tempdir.path().join(format!("seed_{i}.py"));
            std::fs::write(&file, seed.bytes).expect("write seed file");
            let abs = file.to_str().expect("utf8 path").to_owned();
            let id = format!("python:function:{}", seed.qualname);
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, source_file_path, created_at, updated_at \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    id,
                    "python",
                    "function",
                    id,
                    seed.qualname.rsplit('.').next().unwrap_or(seed.qualname),
                    "{}",
                    // A deliberately-wrong stored hash: the read path must NOT
                    // use it (it derives the live whole-file hash instead).
                    "stored-span-hash-not-used",
                    abs,
                    "2026-05-31T00:00:00.000Z",
                    "2026-05-31T00:00:00.000Z",
                ],
            )
            .expect("seed entity row");
            if let Some(blob) = seed.blob {
                conn.execute(
                    "INSERT INTO wardline_taint_facts \
                        (entity_id, wardline_json, scan_id, content_hash_at_compute, updated_at) \
                     VALUES (?1, ?2, NULL, NULL, ?3)",
                    rusqlite::params![id, blob, "2026-05-31T00:00:00.000Z"],
                )
                .expect("seed taint fact");
            }
        }
        // Two seeds may share one file for the dedup test; insert that case
        // explicitly via a shared-file seed below if needed (handled in-test).
        drop(conn);

        let readers = ReaderPool::open(&db_path, 4).expect("open reader pool");
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000007")
                .expect("parse synthetic instance id");
        let state = AppState {
            project_root: tempdir.path().to_path_buf(),
            readers,
            instance_id,
            auth_token: None,
            identity_secret: Some(Arc::new(secret.to_owned())),
            hmac_replay_cache: crate::http_read::auth::new_hmac_replay_cache(),
            taint_writer: None,
        };
        (state, tempdir)
    }

    /// blake3 (hex) of whole file bytes — the contract's `current_content_hash`.
    fn whole_file_blake3(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }

    #[tokio::test]
    async fn wardline_taint_get_returns_fact_with_live_whole_file_hash() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        // Key order b,a is deliberate — RawValue must return it verbatim.
        let blob = r#"{"schema_version":"wardline-taint-1","taint":{"b":2,"a":1}}"#;
        let bytes = b"def f():\n    return 1\n";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "a.b.c",
                bytes,
                blob: Some(blob),
            }],
        );

        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=a.b.c",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("read body");
        let text = String::from_utf8(body.to_vec()).expect("utf8");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("json");

        assert_eq!(parsed["qualname"], "a.b.c");
        assert_eq!(parsed["exists"], true);
        assert_eq!(
            parsed["current_content_hash"],
            whole_file_blake3(bytes),
            "current_content_hash must be the LIVE whole-file blake3"
        );
        // Byte-faithful: the serialized wardline_json sub-object must preserve
        // the original {"b":2,"a":1} key order, not normalize it.
        assert!(
            text.contains(
                r#""wardline_json":{"schema_version":"wardline-taint-1","taint":{"b":2,"a":1}}"#
            ),
            "wardline_json must be byte-faithful (key order preserved): {text}"
        );
    }

    #[tokio::test]
    async fn wardline_taint_get_unknown_qualname_reports_not_exists() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let (state, _tempdir) = wardline_read_test_state(secret, &[]);
        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=does.not.exist",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(parsed["qualname"], "does.not.exist");
        assert_eq!(parsed["exists"], false);
        assert!(
            parsed.get("wardline_json").is_none(),
            "absent fact must omit wardline_json"
        );
        assert!(parsed.get("current_content_hash").is_none());
    }

    /// Finding 2 (corrupt stored blob): an `exists: true` row whose stored
    /// `wardline_json` does not re-parse is a STORAGE-integrity failure, not a
    /// malformed client request. The validated write path (`RawValue` round-trip)
    /// cannot produce this — only storage corruption or an out-of-band write
    /// can — so the test injects it directly via the seed builder's verbatim
    /// blob. The read must return 500 `STORAGE_ERROR` (Loomweave's fault, and 5xx
    /// so `json_read_error` logs it), NOT 400 `INVALID_PATH` (which would blame
    /// the federation client's request for Loomweave's storage damage).
    #[tokio::test]
    async fn wardline_taint_get_corrupt_blob_is_500_storage_error_not_400() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "corrupt.fn",
                bytes: b"def f():\n    return 1\n",
                blob: Some("{not valid json"),
            }],
        );
        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=corrupt.fn",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::INTERNAL_SERVER_ERROR,
            "a corrupt stored blob is Loomweave's fault → 500, never a client 400"
        );
        let bytes = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(
            parsed["code"], "STORAGE_ERROR",
            "corruption must classify as STORAGE_ERROR, not INVALID_PATH"
        );
    }

    #[tokio::test]
    async fn wardline_taint_get_whole_file_hash_not_span_hash() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        // Multi-line file with trailing newline; the function "body" is a
        // strict sub-range so the span hash differs on BOTH axes (span scope +
        // LF normalization). The regression guard for the W.3 bug.
        let bytes = b"line0\nline1\nline2\nline3\n";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "m.span.fn",
                bytes,
                blob: Some(r#"{"v":1}"#),
            }],
        );
        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=m.span.fn",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");

        let whole = whole_file_blake3(bytes);
        // Span-hash formula (analyze.rs::content_hash_for_entity).
        let text = std::str::from_utf8(bytes).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let span = lines[1..3].join("\n");
        let span_hash = blake3::hash(span.as_bytes()).to_hex().to_string();

        assert_eq!(parsed["current_content_hash"], whole);
        assert_ne!(
            parsed["current_content_hash"].as_str().unwrap(),
            span_hash,
            "must be whole-file hash, NOT the span/LF-normalized hash"
        );
    }

    #[tokio::test]
    async fn wardline_taint_batch_get_mixed_present_and_absent() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let bytes = b"def g():\n    pass\n";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "pkg.present",
                bytes,
                blob: Some(r#"{"present":true}"#),
            }],
        );
        let body = br#"{"qualnames":["pkg.present","pkg.absent"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts:batch-get", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes_out = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes_out).expect("json");
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 2, "one view per input qualname, in order");
        assert_eq!(arr[0]["qualname"], "pkg.present");
        assert_eq!(arr[0]["exists"], true);
        assert_eq!(arr[0]["current_content_hash"], whole_file_blake3(bytes));
        assert_eq!(arr[1]["qualname"], "pkg.absent");
        assert_eq!(arr[1]["exists"], false);
        assert!(arr[1].get("wardline_json").is_none());
    }

    /// The qualname RESOLVES to a real entity, but that entity has no stored
    /// taint fact (`blob: None`). This is a distinct path from an unresolved
    /// qualname: both converge on the `exists: false` view, but only this one
    /// exercises `get_taint_facts` returning fewer rows than resolved ids
    /// (present-rows-only). Without this test the changed consumer arm is
    /// covered for "unresolved" but not for "resolved-but-no-fact".
    #[tokio::test]
    async fn wardline_taint_get_resolved_entity_without_fact_reports_not_exists() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "pkg.no_fact",
                bytes: b"def f():\n    pass\n",
                blob: None,
            }],
        );
        let body = br#"{"qualnames":["pkg.no_fact"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts:batch-get", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes_out = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes_out).expect("json");
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["qualname"], "pkg.no_fact");
        assert_eq!(
            arr[0]["exists"], false,
            "resolved entity with no stored fact must report exists: false"
        );
        assert!(arr[0].get("wardline_json").is_none());
        // A resolved-but-no-fact view carries no freshness signal either.
        assert_eq!(arr[0]["current_content_hash"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn wardline_taint_batch_get_shared_file_yields_same_hash() {
        use loomweave_storage::ReaderPool;
        use loomweave_storage::schema::apply_migrations;
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        // Build state by hand so two entities share ONE file (exercises the
        // per-request file-hash dedup; both must report the same hash).
        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("loomweave.db");
        let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
        apply_migrations(&mut conn).expect("migrations");
        let shared = tempdir.path().join("shared.py");
        let bytes: &[u8] = b"def a():\n    pass\n\ndef b():\n    pass\n";
        std::fs::write(&shared, bytes).expect("write shared file");
        let abs = shared.to_str().unwrap().to_owned();
        for q in ["mod.a", "mod.b"] {
            let id = format!("python:function:{q}");
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, source_file_path, created_at, updated_at \
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                rusqlite::params![
                    id,
                    "python",
                    "function",
                    id,
                    q,
                    "{}",
                    "x",
                    abs,
                    "2026-05-31T00:00:00.000Z",
                    "2026-05-31T00:00:00.000Z",
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO wardline_taint_facts \
                    (entity_id, wardline_json, scan_id, content_hash_at_compute, updated_at) \
                 VALUES (?1, ?2, NULL, NULL, ?3)",
                rusqlite::params![id, r#"{"v":1}"#, "2026-05-31T00:00:00.000Z"],
            )
            .unwrap();
        }
        drop(conn);
        let readers = ReaderPool::open(&db_path, 4).expect("reader pool");
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000008")
                .expect("instance id");
        let state = AppState {
            project_root: tempdir.path().to_path_buf(),
            readers,
            instance_id,
            auth_token: None,
            identity_secret: Some(Arc::new(secret.to_owned())),
            hmac_replay_cache: crate::http_read::auth::new_hmac_replay_cache(),
            taint_writer: None,
        };

        let body = br#"{"qualnames":["mod.a","mod.b"]}"#;
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts:batch-get", body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let out = to_bytes(response.into_body(), WARDLINE_BODY_LIMIT_BYTES)
            .await
            .expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&out).expect("json");
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        let expected = whole_file_blake3(bytes);
        assert_eq!(arr[0]["current_content_hash"], expected);
        assert_eq!(
            arr[0]["current_content_hash"], arr[1]["current_content_hash"],
            "two functions in the same file must share one whole-file hash"
        );
    }

    #[tokio::test]
    async fn wardline_taint_batch_get_rejects_oversize_batch() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let (state, _tempdir) = wardline_read_test_state(secret, &[]);
        let qualnames: Vec<String> = (0..=WARDLINE_TAINT_BATCH_MAX)
            .map(|i| format!("pkg.mod.f{i}"))
            .collect();
        assert!(qualnames.len() > WARDLINE_TAINT_BATCH_MAX);
        let body =
            serde_json::to_vec(&serde_json::json!({ "qualnames": qualnames })).expect("json");
        let request = hmac_request(secret, "POST", "/api/wardline/taint-facts:batch-get", &body);
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let bytes = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(parsed["code"], "BATCH_TOO_LARGE");
    }

    #[tokio::test]
    async fn wardline_taint_read_served_with_writer_disabled() {
        use tower::ServiceExt;

        // `wardline_read_test_state` builds `taint_writer: None`. The READ
        // endpoint must still serve (only the WRITE endpoint is gated on it).
        let secret = "wardline-read-secret";
        let bytes = b"def h():\n    pass\n";
        let (state, _tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "x.y.z",
                bytes,
                blob: Some(r#"{"ok":true}"#),
            }],
        );
        assert!(state.taint_writer.is_none(), "write API is disabled");
        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=x.y.z",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "reads must succeed even when the write API is disabled"
        );
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(parsed["exists"], true);
    }

    #[tokio::test]
    async fn wardline_taint_get_deleted_file_yields_none_hash_not_500() {
        use tower::ServiceExt;

        let secret = "wardline-read-secret";
        let bytes = b"def gone():\n    pass\n";
        let (state, tempdir) = wardline_read_test_state(
            secret,
            &[SeedFn {
                qualname: "gone.fn",
                bytes,
                blob: Some(r#"{"v":1}"#),
            }],
        );
        // Delete the containing file: a stale signal → current_content_hash
        // None, fact still reported (exists:true), and NOT a 500.
        std::fs::remove_file(tempdir.path().join("seed_0.py")).expect("remove file");
        let request = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=gone.fn",
            b"",
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.expect("body");
        let parsed: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(parsed["exists"], true);
        assert!(
            parsed.get("current_content_hash").is_none(),
            "deleted file → current_content_hash omitted (None), got: {parsed}"
        );
    }

    // ── Call-graph linkages (Wave 0 / WS2) ───────────────────────────────────

    /// T3.4 end-to-end oracle: a taint fact written before a rename is still
    /// retrievable by its stable SEI after the rename, while a read by the new
    /// locator correctly returns nothing until a re-scan writes under it.
    #[tokio::test]
    async fn wardline_taint_fact_survives_rename_via_sei() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let old = "python:function:old.pkg.fn";
        let new = "python:function:new.pkg.fn";
        let sei = "loomweave:eid:rename-stable";
        // Both pre- and post-rename entity rows exist (entities is cumulative).
        // Alive SEI binding at the OLD locator, as it stands at write time.
        let (state, db_path, _writer, _tempdir) =
            wardline_write_test_state_with_bindings(secret, &[old, new], &[(sei, old)]);

        // 1. Write a fact for the OLD qualname. The request omits `sei`; the
        //    server resolves and stamps it from the alive binding.
        let write_body =
            br#"{"facts":[{"qualname":"old.pkg.fn","wardline_json":{"taint":"EXTERNAL"}}]}"#;
        let write = hmac_request(secret, "POST", "/api/wardline/taint-facts", write_body);
        let resp = router(state.clone()).oneshot(write).await.expect("oneshot");
        let status = resp.status();
        let body = to_bytes(resp.into_body(), 4096).await.expect("body");
        assert_eq!(
            status,
            StatusCode::OK,
            "write response: {}",
            String::from_utf8_lossy(&body)
        );

        // The fact stored under the OLD locator carries the resolved SEI.
        {
            let conn = rusqlite::Connection::open(&db_path).expect("open db");
            let stored: Option<String> = conn
                .query_row(
                    "SELECT sei FROM wardline_taint_facts WHERE entity_id = ?1",
                    rusqlite::params![old],
                    |r| r.get(0),
                )
                .expect("query stored sei");
            assert_eq!(
                stored.as_deref(),
                Some(sei),
                "write must auto-populate the SEI from the alive binding"
            );
        }

        // 2. Simulate the rename: the binding's current_locator flips OLD→NEW.
        {
            let conn = rusqlite::Connection::open(&db_path).expect("open db");
            conn.execute(
                "UPDATE sei_bindings SET current_locator = ?1 WHERE sei = ?2",
                rusqlite::params![new, sei],
            )
            .expect("flip binding locator");
        }

        // 3. read-by-SEI still returns the fact (stored under the dead locator).
        let by_sei_body = serde_json::json!({ "seis": [sei] }).to_string();
        let by_sei = hmac_request(
            secret,
            "POST",
            "/api/wardline/taint-facts/by-sei",
            by_sei_body.as_bytes(),
        );
        let resp = router(state.clone())
            .oneshot(by_sei)
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);
        let parsed = json_body(resp).await;
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 1, "one view per input SEI");
        assert_eq!(arr[0]["sei"], sei);
        assert_eq!(
            arr[0]["exists"], true,
            "the fact survives the rename via its stable SEI"
        );
        assert_eq!(arr[0]["wardline_json"]["taint"], "EXTERNAL");

        // 4. read-by-NEW-locator returns nothing until a re-scan writes there.
        let by_new = hmac_request(
            secret,
            "GET",
            "/api/wardline/taint-facts?qualname=new.pkg.fn",
            b"",
        );
        let resp = router(state.clone())
            .oneshot(by_new)
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            json_body(resp).await["exists"],
            false,
            "no fact under the new locator until re-scan"
        );

        // 5. The post-rename re-scan writes a NEW fact under the NEW locator,
        //    carrying the same (now-current) SEI. read-by-SEI must converge on
        //    the newer fact — genuinely exercising most-recent-across-locators
        //    at the HTTP layer, not just storage.
        let rescan_body =
            br#"{"facts":[{"qualname":"new.pkg.fn","wardline_json":{"taint":"SANITIZED"}}]}"#;
        let rescan = hmac_request(secret, "POST", "/api/wardline/taint-facts", rescan_body);
        let resp = router(state.clone())
            .oneshot(rescan)
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);

        let by_sei_again = hmac_request(
            secret,
            "POST",
            "/api/wardline/taint-facts/by-sei",
            serde_json::json!({ "seis": [sei] }).to_string().as_bytes(),
        );
        let resp = router(state).oneshot(by_sei_again).await.expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);
        let parsed = json_body(resp).await;
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(
            arr[0]["wardline_json"]["taint"], "SANITIZED",
            "by-SEI converges on the freshest fact after re-scan under the new locator"
        );
    }

    /// An unknown SEI yields an honest `exists: false` view, in input order,
    /// never a fabricated row or an error.
    #[tokio::test]
    async fn wardline_taint_by_sei_unknown_reports_not_exists() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, _db_path, _writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:a.b.c"]);
        let body = serde_json::json!({ "seis": ["loomweave:eid:nope"] }).to_string();
        let request = hmac_request(
            secret,
            "POST",
            "/api/wardline/taint-facts/by-sei",
            body.as_bytes(),
        );
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let parsed = json_body(response).await;
        let arr = parsed.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["sei"], "loomweave:eid:nope");
        assert_eq!(arr[0]["exists"], false);
        assert!(arr[0].get("wardline_json").is_none());
    }

    /// The read-by-SEI route is HMAC-gated like the rest of the wardline group.
    #[tokio::test]
    async fn wardline_taint_by_sei_requires_identity() {
        use tower::ServiceExt;

        let secret = "wardline-write-secret";
        let (state, _db_path, _writer, _tempdir) =
            wardline_write_test_state(secret, &["python:function:a.b.c"]);
        // No HMAC signature — a bare request must be rejected.
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/wardline/taint-facts/by-sei")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(r#"{"seis":["loomweave:eid:x"]}"#))
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}
