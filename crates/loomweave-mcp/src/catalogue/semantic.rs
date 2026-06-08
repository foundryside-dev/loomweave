//! `WS5b` — `search_semantic`: embedding-based entity search (ADR-040).
//!
//! Opt-in and honest-degrading: when semantic search is disabled (the default)
//! or no provider is configured, the tool returns an explicit "not enabled"
//! result — never a faked or empty-as-if-complete answer. When enabled it embeds
//! the query, runs a **bounded exact cosine scan** over the git-ignored sidecar
//! (`.weft/loomweave/embeddings.db`), and returns ranked, SEI-carrying entities. Only
//! embeddings whose `content_hash` matches the entity's current hash are
//! considered, so stale vectors never surface (freshness, like the summary
//! cache).

use std::collections::HashMap;

use serde_json::{Value, json};

use loomweave_storage::{EmbeddingStore, embeddings_db_path, entity_by_id};

use crate::ParamError;
use crate::ServerState;
use crate::catalogue::{Page, RawScope, missing_signal};
use crate::{
    entity_json, flatten_storage_envelope_result, required_str, success_envelope,
    tool_error_envelope,
};
use loomweave_core::McpErrorCode;

const SEMANTIC_PAGE_DEFAULT: usize = 20;
const SEMANTIC_PAGE_MAX: usize = 100;
/// Bound on vectors materialised from the sidecar for one cosine scan.
const EMBED_SCAN_CAP: usize = 200_000;

impl ServerState {
    /// `search_semantic(query, limit?, offset?, scope?)` — rank entities by
    /// cosine similarity of their embedding to the query's. Honest "not enabled"
    /// when semantic search is off. Bounded; results carry `sei` + `score`.
    pub(crate) async fn tool_search_semantic(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let query = required_str(arguments, "query")?.to_owned();
        let scope = RawScope::parse(arguments)?;
        let page = Page::parse(arguments, SEMANTIC_PAGE_DEFAULT, SEMANTIC_PAGE_MAX)?;

        // Disabled / unconfigured → honest "not enabled" (never fabricated).
        let Some(state) = self
            .semantic_search
            .as_ref()
            .filter(|state| state.config.enabled)
        else {
            return Ok(success_envelope(json!({
                "result_kind": "not_enabled",
                "results": [],
                "page": { "total": 0, "offset": 0, "limit": 0, "returned": 0, "truncated": false },
                "signal": missing_signal(
                    "semantic_search",
                    "semantic search is not enabled (semantic_search.enabled=false) or no embedding \
                     provider is configured; enable it and run analyze to build embeddings. For \
                     keyword discovery without embeddings, use entity_find — it matches name, \
                     summary, and docstring content by substring (no opt-in required)",
                ),
            })));
        };

        let model_id = state.provider.model_id().to_owned();
        let provider = state.provider.clone();

        // Embed the query using the async provider.
        let embed_query = query.clone();
        let query_vector = match provider.embed(&[embed_query]).await {
            Ok(mut vectors) => match vectors.pop() {
                Some(vector) => vector,
                None => {
                    return Ok(tool_error_envelope(
                        McpErrorCode::LlmProviderError,
                        "embedding provider returned no vector for the query",
                        true,
                    ));
                }
            },
            Err(err) => {
                let retryable = err.retryable();
                return Ok(tool_error_envelope(
                    McpErrorCode::LlmProviderError,
                    &format!("query embedding failed: {err}"),
                    retryable,
                ));
            }
        };

        let project_root = self.project_root.clone();
        let sidecar_path = embeddings_db_path(&project_root);
        let result = self
            .readers
            .with_reader(move |conn| {
                rank_semantic(
                    conn,
                    &project_root,
                    &sidecar_path,
                    &scope,
                    &model_id,
                    &query_vector,
                    page,
                )
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }
}

/// Cosine-rank the sidecar embeddings for `model_id` against `query_vector`,
/// restricted to `scope` and to vectors whose `content_hash` matches the
/// entity's current hash (freshness). Returns a bounded, SEI-carrying envelope.
#[allow(clippy::too_many_arguments)]
fn rank_semantic(
    conn: &rusqlite::Connection,
    project_root: &std::path::Path,
    sidecar_path: &std::path::Path,
    scope: &RawScope,
    model_id: &str,
    query_vector: &[f32],
    page: Page,
) -> loomweave_storage::Result<Value> {
    let filter = scope.resolve(conn)?;
    let (in_scope, scope_truncated) = filter.in_scope_ids(conn, project_root)?;

    let store = EmbeddingStore::open(sidecar_path)?;
    let (rows, scan_truncated) = store.vectors_for_model(model_id, EMBED_SCAN_CAP)?;

    // Cache current content_hash per entity once (freshness gate).
    let mut current_hash: HashMap<String, Option<String>> = HashMap::new();
    let mut scored: Vec<(String, f32)> = Vec::new();
    for row in rows {
        if !in_scope
            .as_ref()
            .is_none_or(|ids| ids.contains(&row.entity_id))
        {
            continue;
        }
        if !current_hash.contains_key(&row.entity_id) {
            let hash = entity_by_id(conn, &row.entity_id)?.and_then(|e| e.content_hash);
            current_hash.insert(row.entity_id.clone(), hash);
        }
        // Freshness: only embeddings of the entity's current content.
        let fresh = current_hash.get(&row.entity_id).and_then(Option::as_deref)
            == Some(row.content_hash.as_str());
        if !fresh {
            continue;
        }
        if let Some(score) = cosine_similarity(query_vector, &row.vector) {
            scored.push((row.entity_id, score));
        }
    }

    // Rank by score desc, ties by id for determinism.
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let total = scored.len();
    let returned: Vec<(String, f32)> = scored
        .into_iter()
        .skip(page.offset)
        .take(page.limit)
        .collect();
    let returned_count = returned.len();
    let truncated = page.offset.saturating_add(returned_count) < total;

    let results: Vec<Value> = returned
        .iter()
        .map(|(id, score)| {
            let entity = match entity_by_id(conn, id) {
                Ok(Some(entity)) => entity_json(conn, &entity),
                _ => json!({ "id": id, "sei": Value::Null }),
            };
            json!({ "entity": entity, "score": score })
        })
        .collect();

    Ok(success_envelope(json!({
        "result_kind": "ranked",
        "model_id": model_id,
        "results": results,
        "page": {
            "total": total,
            "offset": page.offset,
            "limit": page.limit,
            "returned": returned_count,
            "truncated": truncated,
        },
        "scope_truncated": scope_truncated,
        "scan_truncated": scan_truncated,
    })))
}

/// Cosine similarity of two equal-length vectors. `None` when lengths differ or
/// either vector has zero magnitude (no meaningful angle).
fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return None;
    }
    Some(dot / (norm_a.sqrt() * norm_b.sqrt()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_of_identical_unit_vectors_is_one() {
        let s = cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]).unwrap();
        assert!((s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_of_orthogonal_is_zero() {
        let s = cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).unwrap();
        assert!(s.abs() < 1e-6);
    }

    #[test]
    fn cosine_rejects_mismatched_or_zero() {
        assert!(cosine_similarity(&[1.0], &[1.0, 2.0]).is_none());
        assert!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]).is_none());
    }
}
