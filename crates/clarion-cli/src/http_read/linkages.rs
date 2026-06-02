//! Call-graph linkage endpoints (callers / callees) and aggregation helpers.
//!
//! Split out of `http_read.rs` (mechanical relocation; behaviour unchanged).

use anyhow::Result;
use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use clarion_core::EdgeConfidence;
use clarion_core::HttpErrorCode as ErrorCode;
use clarion_storage::{
    CallEdgeMatch, EntityVisibility, StorageError, call_edges_from, call_edges_targeting,
    entity_visibility,
};
use serde::{Deserialize, Serialize};

use super::errors::json_read_error;
use super::{AppState, json_error};

/// Default / maximum page size for the call-graph linkage routes (WS2).
pub(crate) const LINKAGE_LIMIT_DEFAULT: u32 = 50;

pub(crate) const LINKAGE_LIMIT_MAX: u32 = 200;

/// Max entity ids in one linkage `:batch-get` request. Pinned in contracts.md;
/// clients split oversize sets client-side (mirrors `BATCH_MAX_QUERIES`).
pub(crate) const LINKAGES_BATCH_MAX: usize = 50;

#[derive(Debug, Serialize)]
pub(crate) struct LinkageEntry {
    entity_id: String,
    confidence: String,
    call_site_count: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinkageQuery {
    #[serde(default)]
    confidence: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    offset: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinkageBatchRequest {
    entity_ids: Vec<String>,
    #[serde(default)]
    confidence: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

/// Which side of the `calls` graph a linkage query walks.
#[derive(Debug, Clone, Copy)]
pub(crate) enum LinkageDirection {
    /// Inbound: who calls the entity (neighbour = caller `from_id`).
    Callers,
    /// Outbound: what the entity calls (neighbour = callee `to_id`).
    Callees,
}

impl LinkageDirection {
    /// The JSON field name the page of entries is serialised under.
    fn field(self) -> &'static str {
        match self {
            LinkageDirection::Callers => "callers",
            LinkageDirection::Callees => "callees",
        }
    }
}

/// Map the `confidence` query value to the storage layer's `max_confidence`
/// CEILING (`confidence_allowed` is `actual <= max`). `all` and `inferred` both
/// admit every tier; `ambiguous` admits resolved+ambiguous; `resolved` admits
/// only resolved. Returns `None` for an unrecognised value (→ 400).
pub(crate) fn parse_max_confidence(raw: Option<&str>) -> Option<EdgeConfidence> {
    match raw.unwrap_or("all") {
        "all" | "inferred" => Some(EdgeConfidence::Inferred),
        "ambiguous" => Some(EdgeConfidence::Ambiguous),
        "resolved" => Some(EdgeConfidence::Resolved),
        _ => None,
    }
}

/// Rank for "strongest tier" selection: lower is more certain.
pub(crate) fn confidence_rank(confidence: EdgeConfidence) -> u8 {
    match confidence {
        EdgeConfidence::Resolved => 0,
        EdgeConfidence::Ambiguous => 1,
        EdgeConfidence::Inferred => 2,
    }
}

/// Aggregate raw call-edge matches into per-neighbour [`LinkageEntry`] rows,
/// deterministically ordered by `entity_id`. The neighbour is the `from_id`
/// for callers and the `to_id` for callees (`call_edges_from` already expands an
/// ambiguous edge into one match per candidate callee, so the callee count
/// reflects candidate breadth — faithful to the wrapped query).
pub(crate) fn aggregate_linkages(
    matches: &[CallEdgeMatch],
    direction: LinkageDirection,
) -> Vec<LinkageEntry> {
    use std::collections::BTreeMap;
    let mut acc: BTreeMap<&str, (EdgeConfidence, usize)> = BTreeMap::new();
    for edge in matches {
        let neighbour = match direction {
            LinkageDirection::Callers => edge.from_id.as_str(),
            LinkageDirection::Callees => edge.to_id.as_str(),
        };
        let slot = acc.entry(neighbour).or_insert((edge.confidence, 0));
        slot.1 += 1;
        if confidence_rank(edge.confidence) < confidence_rank(slot.0) {
            slot.0 = edge.confidence;
        }
    }
    acc.into_iter()
        .map(|(entity_id, (confidence, call_site_count))| LinkageEntry {
            entity_id: entity_id.to_owned(),
            confidence: confidence.as_str().to_owned(),
            call_site_count,
        })
        .collect()
}

/// Fetch + aggregate one entity's linkages on a pooled connection. Returns the
/// full (unpaginated) ordered entry list, distinguishing not-found / blocked.
pub(crate) fn linkages_for(
    conn: &rusqlite::Connection,
    entity_id: &str,
    direction: LinkageDirection,
    max_confidence: EdgeConfidence,
) -> Result<LinkageLookup, StorageError> {
    match entity_visibility(conn, entity_id)? {
        EntityVisibility::NotFound => Ok(LinkageLookup::NotFound),
        EntityVisibility::Blocked(_) => Ok(LinkageLookup::Blocked),
        EntityVisibility::Visible => {
            let matches = match direction {
                LinkageDirection::Callers => call_edges_targeting(conn, entity_id, max_confidence)?,
                LinkageDirection::Callees => call_edges_from(conn, entity_id, max_confidence)?,
            };
            Ok(LinkageLookup::Found(aggregate_linkages(
                &matches, direction,
            )))
        }
    }
}

pub(crate) enum LinkageLookup {
    NotFound,
    Blocked,
    Found(Vec<LinkageEntry>),
}

/// Single-entity linkage handler (callers or callees).
pub(crate) async fn linkage_single(
    state: &AppState,
    entity_id: String,
    query: Result<Query<LinkageQuery>, QueryRejection>,
    direction: LinkageDirection,
) -> Response {
    let Ok(Query(query)) = query else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "query parameters are invalid",
        );
    };
    let Some(max_confidence) = parse_max_confidence(query.confidence.as_deref()) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "confidence must be one of: resolved, ambiguous, inferred, all",
        );
    };
    let limit = query
        .limit
        .unwrap_or(LINKAGE_LIMIT_DEFAULT)
        .min(LINKAGE_LIMIT_MAX) as usize;
    let offset = query.offset.unwrap_or(0) as usize;

    let lookup_id = entity_id.clone();
    let result = state
        .readers
        .with_reader(move |conn| linkages_for(conn, &lookup_id, direction, max_confidence))
        .await;

    match result {
        Ok(LinkageLookup::NotFound) => json_error(
            StatusCode::NOT_FOUND,
            ErrorCode::NotFound,
            "entity is not known to Clarion",
        ),
        Ok(LinkageLookup::Blocked) => json_error(
            StatusCode::FORBIDDEN,
            ErrorCode::BriefingBlocked,
            "entity is briefing-blocked and cannot be exposed",
        ),
        Ok(LinkageLookup::Found(entries)) => {
            let total = entries.len();
            let page: Vec<LinkageEntry> = entries.into_iter().skip(offset).take(limit).collect();
            let truncated = offset.saturating_add(page.len()) < total;
            let body = serde_json::json!({
                "entity_id": entity_id,
                direction.field(): page,
                "total": total,
                "truncated": truncated,
            });
            (StatusCode::OK, Json(body)).into_response()
        }
        Err(err) => json_read_error(&err),
    }
}

/// Batch linkage handler. Returns `{ results: { entity_id: [LinkageEntry] } }`
/// for the VISIBLE requested ids only; not-found and briefing-blocked ids are
/// omitted (the caller diffs requested vs returned keys). `limit` caps entries
/// per entity; there is no offset on the batch surface.
pub(crate) async fn linkage_batch(
    state: &AppState,
    body: Result<Json<LinkageBatchRequest>, axum::extract::rejection::JsonRejection>,
    direction: LinkageDirection,
) -> Response {
    let Ok(Json(request)) = body else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "request body must be a JSON object {\"entity_ids\": [...]}",
        );
    };
    if request.entity_ids.len() > LINKAGES_BATCH_MAX {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::BatchTooLarge,
            "entity_ids[] exceeds the per-batch maximum of 50 entries",
        );
    }
    let Some(max_confidence) = parse_max_confidence(request.confidence.as_deref()) else {
        return json_error(
            StatusCode::BAD_REQUEST,
            ErrorCode::InvalidPath,
            "confidence must be one of: resolved, ambiguous, inferred, all",
        );
    };
    let limit = request
        .limit
        .unwrap_or(LINKAGE_LIMIT_DEFAULT)
        .min(LINKAGE_LIMIT_MAX) as usize;
    let entity_ids = request.entity_ids;

    let result = state
        .readers
        .with_reader(move |conn| {
            // BTreeMap → deterministic JSON object key order.
            let mut results: std::collections::BTreeMap<String, Vec<LinkageEntry>> =
                std::collections::BTreeMap::new();
            for entity_id in entity_ids {
                if let LinkageLookup::Found(mut entries) =
                    linkages_for(conn, &entity_id, direction, max_confidence)?
                {
                    entries.truncate(limit);
                    results.insert(entity_id, entries);
                }
            }
            Ok::<_, StorageError>(results)
        })
        .await;

    match result {
        Ok(results) => (
            StatusCode::OK,
            Json(serde_json::json!({ "results": results })),
        )
            .into_response(),
        Err(err) => json_read_error(&err),
    }
}

pub(crate) async fn get_callers(
    State(state): State<AppState>,
    Path(entity_id): Path<String>,
    query: Result<Query<LinkageQuery>, QueryRejection>,
) -> Response {
    linkage_single(&state, entity_id, query, LinkageDirection::Callers).await
}

pub(crate) async fn get_callees(
    State(state): State<AppState>,
    Path(entity_id): Path<String>,
    query: Result<Query<LinkageQuery>, QueryRejection>,
) -> Response {
    linkage_single(&state, entity_id, query, LinkageDirection::Callees).await
}

pub(crate) async fn post_callers_batch(
    State(state): State<AppState>,
    body: Result<Json<LinkageBatchRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    linkage_batch(&state, body, LinkageDirection::Callers).await
}

pub(crate) async fn post_callees_batch(
    State(state): State<AppState>,
    body: Result<Json<LinkageBatchRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    linkage_batch(&state, body, LinkageDirection::Callees).await
}

// ── SEI identity resolution (Wave 1 / WS1, ADR-038 §4 / SEI spec §4) ─────────
//
// `resolve(locator)`     → the alive SEI for a locator, or { alive: false }.
// `resolve_sei(sei)`     → the alive record, or { alive: false, lineage }.
// `lineage(sei)`         → the ordered event list.
// All HMAC-gated like `/api/v1/files`. Identity is read from `sei_bindings`
// (the source of truth); `entities` is joined only for `content_hash`.
//
// REQ-F-02 (fail-closed): `resolve(locator)` MUST reject an SEI-shaped input
// (reserved `clarion:eid:` prefix) — never silently mis-resolve. A colon-count
// check is insufficient (an SEI carries the same two colons a locator does), so
// the rejection keys on the reserved prefix. This is what makes the idempotent,
// resumable cross-tool backfill safe (an already-migrated SEI is rejected).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_read::router;
    use crate::http_read::test_support::{hmac_request, json_body};
    use std::sync::Arc;

    #[test]
    fn aggregate_linkages_picks_strongest_tier_and_counts_sites() {
        // Pure aggregation contract: per neighbour, `call_site_count` spans tiers
        // and `confidence` is the STRONGEST present. Caller `a` has an ambiguous
        // and a resolved site → resolved wins, count 2; `b` has one inferred site.
        let mk = |from: &str, conf: EdgeConfidence, start: i64| CallEdgeMatch {
            from_id: from.to_owned(),
            to_id: "python:function:t".to_owned(),
            stored_to_id: "python:function:t".to_owned(),
            confidence: conf,
            source_file_id: None,
            source_byte_start: Some(start),
            source_byte_end: Some(start + 5),
            properties_json: None,
        };
        let matches = vec![
            mk("python:function:a", EdgeConfidence::Ambiguous, 0),
            mk("python:function:a", EdgeConfidence::Resolved, 10),
            mk("python:function:b", EdgeConfidence::Inferred, 20),
        ];
        let entries = aggregate_linkages(&matches, LinkageDirection::Callers);
        assert_eq!(entries.len(), 2);
        // BTreeMap ordering → deterministic by entity_id.
        assert_eq!(entries[0].entity_id, "python:function:a");
        assert_eq!(entries[0].confidence, "resolved");
        assert_eq!(entries[0].call_site_count, 2);
        assert_eq!(entries[1].entity_id, "python:function:b");
        assert_eq!(entries[1].confidence, "inferred");
        assert_eq!(entries[1].call_site_count, 1);
    }

    #[test]
    fn parse_max_confidence_maps_tiers_and_rejects_garbage() {
        assert_eq!(parse_max_confidence(None), Some(EdgeConfidence::Inferred));
        assert_eq!(
            parse_max_confidence(Some("all")),
            Some(EdgeConfidence::Inferred)
        );
        assert_eq!(
            parse_max_confidence(Some("inferred")),
            Some(EdgeConfidence::Inferred)
        );
        assert_eq!(
            parse_max_confidence(Some("ambiguous")),
            Some(EdgeConfidence::Ambiguous)
        );
        assert_eq!(
            parse_max_confidence(Some("resolved")),
            Some(EdgeConfidence::Resolved)
        );
        assert_eq!(parse_max_confidence(Some("bogus")), None);
    }

    /// Build an `AppState` over a temp DB seeded with entities + `calls` edges.
    /// `entities`: (id, `briefing_blocked` reason?). `calls`: (from, to,
    /// confidence, `candidate_ids`). Carries an HMAC secret so the protected
    /// routes verify real signatures.
    fn linkage_test_state(
        secret: &str,
        entities: &[(&str, Option<&str>)],
        calls: &[(&str, &str, &str, &[&str])],
    ) -> (AppState, tempfile::TempDir) {
        use clarion_storage::ReaderPool;
        use clarion_storage::schema::apply_migrations;

        let tempdir = tempfile::tempdir().expect("temp project root");
        let db_path = tempdir.path().join("clarion.db");
        let mut conn = rusqlite::Connection::open(&db_path).expect("open db");
        apply_migrations(&mut conn).expect("apply migrations");
        for (id, blocked) in entities {
            let properties = match blocked {
                Some(reason) => serde_json::json!({ "briefing_blocked": reason }).to_string(),
                None => "{}".to_owned(),
            };
            conn.execute(
                "INSERT INTO entities ( \
                    id, plugin_id, kind, name, short_name, properties, \
                    content_hash, created_at, updated_at \
                 ) VALUES (?1, 'python', 'function', ?1, ?1, ?2, 'deadbeef', \
                    '2026-05-31T00:00:00.000Z', '2026-05-31T00:00:00.000Z')",
                rusqlite::params![id, properties],
            )
            .expect("seed entity row");
        }
        for (idx, (from, to, confidence, candidates)) in calls.iter().enumerate() {
            let properties: Option<String> = if candidates.is_empty() {
                None
            } else {
                Some(serde_json::json!({ "candidates": candidates }).to_string())
            };
            let byte = i64::try_from(idx).expect("test edge index fits i64") * 10;
            conn.execute(
                "INSERT INTO edges ( \
                    kind, from_id, to_id, properties, source_file_id, \
                    source_byte_start, source_byte_end, confidence \
                 ) VALUES ('calls', ?1, ?2, ?3, NULL, ?4, ?5, ?6)",
                rusqlite::params![from, to, properties, byte, byte + 5, confidence],
            )
            .expect("seed calls edge");
        }
        drop(conn);

        let readers = ReaderPool::open(&db_path, 4).expect("open reader pool");
        let instance_id =
            crate::instance::parse_instance_id_for_test("00000000-0000-4000-8000-000000000005")
                .expect("parse synthetic instance id");
        let state = AppState {
            project_root: tempdir.path().to_path_buf(),
            readers,
            instance_id,
            auth_token: None,
            identity_secret: Some(Arc::new(secret.to_owned())),
            taint_writer: None,
        };
        (state, tempdir)
    }

    #[tokio::test]
    async fn linkage_callers_returns_aggregated_neighbours() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        // a -> t (resolved); b -> t (ambiguous, t is a candidate).
        let (state, _tempdir) = linkage_test_state(
            secret,
            &[
                ("python:function:t", None),
                ("python:function:a", None),
                ("python:function:b", None),
            ],
            &[
                ("python:function:a", "python:function:t", "resolved", &[]),
                (
                    "python:function:b",
                    "python:function:t",
                    "ambiguous",
                    &["python:function:t"],
                ),
            ],
        );
        let path = "/api/v1/entities/python:function:t/callers?confidence=all";
        let response = router(state)
            .oneshot(hmac_request(secret, "GET", path, b""))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let parsed = json_body(response).await;
        assert_eq!(parsed["entity_id"], "python:function:t");
        assert_eq!(parsed["total"], 2);
        assert_eq!(parsed["truncated"], false);
        let callers = parsed["callers"].as_array().expect("callers array");
        let by_id: std::collections::HashMap<&str, &serde_json::Value> = callers
            .iter()
            .map(|c| (c["entity_id"].as_str().unwrap(), c))
            .collect();
        assert_eq!(by_id["python:function:a"]["confidence"], "resolved");
        assert_eq!(by_id["python:function:a"]["call_site_count"], 1);
        assert_eq!(by_id["python:function:b"]["confidence"], "ambiguous");
    }

    #[tokio::test]
    async fn linkage_callees_returns_outbound_calls() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(
            secret,
            &[("python:function:s", None), ("python:function:x", None)],
            &[("python:function:s", "python:function:x", "resolved", &[])],
        );
        let path = "/api/v1/entities/python:function:s/callees";
        let response = router(state)
            .oneshot(hmac_request(secret, "GET", path, b""))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let parsed = json_body(response).await;
        let callees = parsed["callees"].as_array().expect("callees array");
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0]["entity_id"], "python:function:x");
        assert_eq!(callees[0]["confidence"], "resolved");
    }

    #[tokio::test]
    async fn linkage_unknown_entity_is_404() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(secret, &[("python:function:t", None)], &[]);
        let path = "/api/v1/entities/python:function:nope/callers";
        let response = router(state)
            .oneshot(hmac_request(secret, "GET", path, b""))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(response).await["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn linkage_confidence_filter_excludes_weaker_tiers() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let seed = |s: &str| {
            linkage_test_state(
                s,
                &[
                    ("python:function:t", None),
                    ("python:function:a", None),
                    ("python:function:b", None),
                ],
                &[
                    ("python:function:a", "python:function:t", "resolved", &[]),
                    (
                        "python:function:b",
                        "python:function:t",
                        "ambiguous",
                        &["python:function:t"],
                    ),
                ],
            )
        };

        // resolved-only: the ambiguous caller `b` must be excluded.
        let (state, _t1) = seed(secret);
        let resolved = router(state)
            .oneshot(hmac_request(
                secret,
                "GET",
                "/api/v1/entities/python:function:t/callers?confidence=resolved",
                b"",
            ))
            .await
            .expect("oneshot");
        let resolved = json_body(resolved).await;
        assert_eq!(
            resolved["total"], 1,
            "resolved filter excludes ambiguous: {resolved}"
        );
        assert_eq!(resolved["callers"][0]["entity_id"], "python:function:a");

        // all: both tiers present.
        let (state, _t2) = seed(secret);
        let all = router(state)
            .oneshot(hmac_request(
                secret,
                "GET",
                "/api/v1/entities/python:function:t/callers?confidence=all",
                b"",
            ))
            .await
            .expect("oneshot");
        assert_eq!(json_body(all).await["total"], 2);
    }

    #[tokio::test]
    async fn linkage_invalid_confidence_is_400() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(secret, &[("python:function:t", None)], &[]);
        let response = router(state)
            .oneshot(hmac_request(
                secret,
                "GET",
                "/api/v1/entities/python:function:t/callers?confidence=bogus",
                b"",
            ))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn linkage_briefing_blocked_queried_entity_is_403() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        // Queried entity carries a briefing-block marker → refuse (mirrors get_file).
        let (state, _tempdir) = linkage_test_state(
            secret,
            &[("python:function:secret", Some("secret-scan"))],
            &[],
        );
        let response = router(state)
            .oneshot(hmac_request(
                secret,
                "GET",
                "/api/v1/entities/python:function:secret/callers",
                b"",
            ))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(json_body(response).await["code"], "BRIEFING_BLOCKED");
    }

    #[tokio::test]
    async fn linkage_requires_authentication() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(secret, &[("python:function:t", None)], &[]);
        // No X-Loom-Component header → 401 (route is HMAC-gated like /api/v1/files).
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/v1/entities/python:function:t/callers")
            .body(axum::body::Body::empty())
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn linkage_batch_returns_only_known_visible_entities() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(
            secret,
            &[
                ("python:function:t", None),
                ("python:function:a", None),
                ("python:function:blocked", Some("secret-scan")),
            ],
            &[("python:function:a", "python:function:t", "resolved", &[])],
        );
        // t (known), blocked (briefing-blocked), nope (unknown) → results has t only.
        let body = br#"{"entity_ids":["python:function:t","python:function:blocked","python:function:nope"]}"#;
        let response = router(state)
            .oneshot(hmac_request(
                secret,
                "POST",
                "/api/v1/entities/callers:batch-get",
                body,
            ))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        let parsed = json_body(response).await;
        let results = parsed["results"].as_object().expect("results object");
        assert_eq!(
            results.len(),
            1,
            "only the known visible entity appears: {parsed}"
        );
        assert_eq!(
            results["python:function:t"][0]["entity_id"],
            "python:function:a"
        );
    }

    #[tokio::test]
    async fn linkage_batch_over_limit_is_400() {
        use tower::ServiceExt;
        let secret = "linkage-secret";
        let (state, _tempdir) = linkage_test_state(secret, &[("python:function:t", None)], &[]);
        let ids: Vec<String> = (0..=LINKAGES_BATCH_MAX)
            .map(|i| format!("python:function:e{i}"))
            .collect();
        let body = serde_json::json!({ "entity_ids": ids }).to_string();
        let response = router(state)
            .oneshot(hmac_request(
                secret,
                "POST",
                "/api/v1/entities/callees:batch-get",
                body.as_bytes(),
            ))
            .await
            .expect("oneshot");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(json_body(response).await["code"], "BATCH_TOO_LARGE");
    }

    #[tokio::test]
    async fn capabilities_reports_linkages_http_true() {
        use tower::ServiceExt;
        // _capabilities is unauthenticated so siblings can probe pre-auth.
        let (state, _tempdir) = linkage_test_state("linkage-secret", &[], &[]);
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/v1/_capabilities")
            .body(axum::body::Body::empty())
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(json_body(response).await["linkages"]["http"], true);
    }

    #[tokio::test]
    async fn capabilities_reports_taint_store_read_by_sei_true() {
        use tower::ServiceExt;
        // Discrete from `sei.supported`: an older SEI-capable Clarion would set
        // `sei.supported: true` yet lack this route, so consumers gate the
        // rename-stable taint read on this flag specifically.
        let (state, _tempdir) = linkage_test_state("linkage-secret", &[], &[]);
        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/api/v1/_capabilities")
            .body(axum::body::Body::empty())
            .expect("build request");
        let response = router(state).oneshot(request).await.expect("oneshot");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await["taint_store"]["read_by_sei"],
            true
        );
    }
}
