//! Status, source & diagnostics reads: `source_for_entity`, `summary_preview_cost`, `project_status`, `index_diff`.
//!
//! Extracted from `lib.rs` (V11-ARCH-04). Methods attach to
//! [`crate::ServerState`] via an inherent `impl` block; `lib.rs` keeps the
//! shared free-function helpers, the tool catalogue, and the JSON-RPC dispatch.

use std::collections::HashMap;

use loomweave_core::{LeafSummaryPromptInput, McpErrorCode, build_leaf_summary_prompt};
use serde_json::{Value, json};

use loomweave_storage::{StorageError, contained_entity_ids, entity_by_id, has_any_alive_binding};

use crate::{
    IssuesForRead, ParamError, SUMMARY_MAX_OUTPUT_TOKENS, ServerState, SummaryRead, entity_json,
    estimate_tokens_from_chars, flatten_storage_envelope_result, latest_run_row, optional_usize,
    plugin_entity_counts, required_str, scalar_count_fail_soft, source_for_entity_json,
    storage_retryable, success_envelope, summary_cache_expired, summary_read_error,
    timestamp_day_index, tool_error_envelope, verified_source_excerpt,
};

impl ServerState {
    pub(crate) async fn tool_source_for_entity(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        // Bounded context window; the schema caps at 200 but clamp defensively.
        let context_lines = optional_usize(arguments, "context_lines")?
            .unwrap_or(10)
            .min(200);
        let id_for_reader = entity_id.clone();
        // Build the payload (including the entity's SEI read-time join) inside
        // the reader closure so a connection is in scope for `entity_json`.
        let payload = self
            .readers
            .with_reader(move |conn| {
                let Some(entity) = entity_by_id(conn, &id_for_reader)? else {
                    return Ok(None);
                };
                Ok(Some(source_for_entity_json(conn, &entity, context_lines)))
            })
            .await;
        match payload {
            Ok(Some(payload)) => Ok(success_envelope(payload)),
            Ok(None) => Ok(tool_error_envelope(
                McpErrorCode::NotFound,
                &format!("no entity with id {entity_id}"),
                false,
            )),
            Err(err) => Ok(tool_error_envelope(
                McpErrorCode::StorageError,
                &err.to_string(),
                storage_retryable(&err),
            )),
        }
    }

    pub(crate) async fn tool_summary_preview_cost(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let now = (self.clock)();
        let read = match self
            .read_summary_inputs(entity_id, self.summary_model_id(), now.clone())
            .await
        {
            Ok(read) => read,
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };
        // Non-summarizable entities (missing, subsystem, briefing-blocked,
        // no-content-hash) reuse the same reasons summary() reports.
        let SummaryRead::Ready(ready) = read else {
            return Ok(summary_read_error(read));
        };

        // LLM policy posture (no provider call). `live` means a provider is
        // wired AND config permits it; that is what makes a miss spend. A
        // disabled/unconfigured LLM is therefore distinct from a cache miss.
        let llm_enabled = self
            .summary_llm
            .as_ref()
            .is_some_and(|llm| llm.config.enabled);
        let live = self.summary_llm.is_some() && llm_enabled;
        let allow_live_provider = self
            .summary_llm
            .as_ref()
            .is_some_and(|llm| llm.config.allow_live_provider);
        let provider = self.diagnostics.as_ref().map_or_else(
            || if live { "configured" } else { "disabled" }.to_owned(),
            |diag| diag.llm.provider.clone(),
        );

        // Cache status without spending: a fresh row is a hit; a present-but-
        // expired row would be re-billed; absence is a miss.
        let (cache_status, cached_json) = match ready.cached.as_ref() {
            Some(cached) => {
                let expired = summary_cache_expired(
                    &cached.created_at,
                    &now,
                    self.summary_cache_max_age_days(),
                );
                let age_days = timestamp_day_index(&now)
                    .zip(timestamp_day_index(&cached.created_at))
                    .map(|(current, created)| current.saturating_sub(created));
                let json = json!({
                    "created_at": cached.created_at,
                    "last_accessed_at": cached.last_accessed_at,
                    "age_days": age_days,
                    "model_id": cached.key.model_tier,
                    "tokens_input": cached.tokens_input,
                    "tokens_output": cached.tokens_output,
                    "cost_usd": cached.cost_usd,
                    "stale_semantic": cached.stale_semantic,
                });
                (if expired { "expired" } else { "hit" }, json)
            }
            None => ("miss", Value::Null),
        };

        // On a miss/expired row a fresh call estimates input tokens from the
        // leaf prompt (chars/4 heuristic — no provider, no spend). A hit needs
        // no estimate: the cached row already carries the real token counts.
        let estimated_input_tokens = if cache_status == "hit" {
            None
        } else {
            verified_source_excerpt(&ready.entity)
                .ok()
                .map(|source_excerpt| {
                    let prompt = build_leaf_summary_prompt(&LeafSummaryPromptInput {
                        entity_id: ready.entity.id.clone(),
                        kind: ready.entity.kind.clone(),
                        name: ready.entity.name.clone(),
                        guidance: ready.guidance_text.clone(),
                        source_excerpt,
                    });
                    estimate_tokens_from_chars(&prompt.body)
                })
        };

        let live_spend_would_occur = cache_status != "hit" && live;

        Ok(success_envelope(json!({
            "entity": {"id": ready.entity.id, "kind": ready.entity.kind},
            "cache_status": cache_status,
            "cached": cached_json,
            "model_id": self.summary_model_id(),
            "estimated_input_tokens": estimated_input_tokens,
            // summary() caps output at 512 tokens; report it as the ceiling, not
            // a prediction of actual output length.
            "estimated_output_tokens": SUMMARY_MAX_OUTPUT_TOKENS,
            // No per-model pricing table at v1.0 — cost is reported only for
            // cache hits/expired rows (the cached row carries a real cost_usd).
            "estimated_cost_usd": Value::Null,
            "policy": {
                "enabled": llm_enabled,
                "live": live,
                "allow_live_provider": allow_live_provider,
                "provider": provider,
                "cache_max_age_days": self.summary_cache_max_age_days(),
            },
            "live_spend_would_occur": live_spend_would_occur,
        })))
    }

    pub(crate) async fn tool_project_status(
        &self,
        _arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let db_path = self.project_root.join(".loomweave").join("loomweave.db");
        let root_display = self.project_root.display().to_string();

        let project_root = self.project_root.clone();
        let storage = self
            .readers
            .with_reader(move |conn| {
                let snapshot = crate::snapshot::project_snapshot(conn, &project_root);
                let edge_count = scalar_count_fail_soft(conn, "SELECT COUNT(*) FROM edges");
                // Entities withheld from briefings/federation exposure (secret
                // scan set `briefing_blocked`). Served by the partial index
                // ix_entities_briefing_blocked over the generated column
                // (clarion-bdabfd6bca) — no per-row JSON parse.
                let briefing_blocked = scalar_count_fail_soft(
                    conn,
                    "SELECT COUNT(*) FROM entities WHERE briefing_blocked IS NOT NULL",
                );
                let plugins = plugin_entity_counts(conn);
                let latest_run = latest_run_row(conn);
                // SQLite's data_version increments when another connection commits
                // to the DB, so a consult agent can detect that the index changed
                // under it across calls (clarion-22c18fdb34).
                let data_version = scalar_count_fail_soft(conn, "PRAGMA data_version");
                // Whether this index has any alive SEI bindings (REQ-C-04 /
                // ADR-038). Degrades to `false` on a pre-SEI database.
                let sei_populated = has_any_alive_binding(conn).unwrap_or(false);
                Ok((
                    snapshot,
                    edge_count,
                    briefing_blocked,
                    plugins,
                    latest_run,
                    data_version,
                    sei_populated,
                ))
            })
            .await;

        let (
            snapshot,
            edge_count,
            briefing_blocked,
            plugins,
            latest_run,
            data_version,
            sei_populated,
        ) = match storage {
            Ok(tuple) => tuple,
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };

        // The on-disk size, paired with data_version, exposes a swapped or
        // truncated DB the server may still be serving from a stale handle.
        let db_size_bytes = std::fs::metadata(&db_path).map(|meta| meta.len()).ok();
        let analyzed_git_sha = latest_run
            .get("analyzed_at_commit")
            .cloned()
            .unwrap_or(Value::Null);

        // A served index that has a completed run but no entities is almost
        // always a wrong/empty/swapped corpus — surface it in the log so an
        // operator notices even without reading the diagnostics (clarion-22c18fdb34).
        if snapshot.db_present()
            && snapshot.entity_count() == 0
            && snapshot.last_analyzed_at().is_some()
        {
            tracing::warn!(
                db_path = %db_path.display(),
                "project_status: served index has a completed run but zero entities (possible empty or swapped DB)"
            );
        }

        // Disclose what a `fresh` verdict does NOT cover, on the named tool an
        // agent reads directly — not just in the session-start banner
        // (clarion-26c7e52027). `fresh` compares already-indexed files' mtimes; a
        // brand-new module in a not-yet-indexed top-level directory, or any
        // uncommitted addition (undetectable on an untrusted corpus), can sit
        // unseen behind it. `index_diff_get` reports committed/staged drift in
        // detail (it shares the untracked blind spot); re-analyze is the remedy.
        let staleness_note = match snapshot.staleness() {
            crate::snapshot::Staleness::Fresh => Some(
                "\"fresh\" reflects already-indexed source files only; it does NOT detect \
                 brand-new modules in a not-yet-indexed directory, nor uncommitted \
                 additions. If source was added or moved since the last analyze, re-run \
                 `loomweave analyze`. Use index_diff_get for committed/staged drift detail.",
            ),
            crate::snapshot::Staleness::StaleWorktree => Some(
                "the working tree has untracked source files of already-indexed types that \
                 the index has not seen (new modules not yet analyzed; see worktree_dirty). \
                 Re-run `loomweave analyze` before relying on graph answers.",
            ),
            _ => None,
        };

        let result = json!({
            "project_root": root_display,
            "db_path": db_path.display().to_string(),
            "db_present": snapshot.db_present(),
            "db_identity": {
                "db_size_bytes": db_size_bytes,
                "data_version": data_version,
            },
            "latest_run": latest_run,
            "counts": {
                "entities": snapshot.entity_count(),
                "subsystems": snapshot.subsystem_count(),
                "edges": edge_count,
                "findings": snapshot.finding_count(),
                "briefing_blocked": briefing_blocked,
            },
            "staleness": serde_json::to_value(snapshot.staleness()).unwrap_or(Value::Null),
            "staleness_note": staleness_note,
            "worktree_dirty": snapshot.worktree_dirty(),
            "scan_truncated": snapshot.scan_truncated(),
            "last_analyzed_at": snapshot.last_analyzed_at(),
            "git_sha": analyzed_git_sha,
            "plugins": plugins,
            // Whether this build understands SEIs (always true here) and whether
            // the served index actually has SEI bindings populated (REQ-C-04 /
            // ADR-038). A consult agent reads this to know if entity responses
            // will carry a non-null `sei`.
            "sei": {
                "supported": true,
                "populated": sei_populated,
            },
            "llm": self.llm_diagnostics_json(),
            "filigree": self.filigree_diagnostics_json(),
            "loomweave_read_api": self.loomweave_read_api_json(),
        });

        Ok(success_envelope(result))
    }

    pub(crate) async fn tool_index_diff(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let cap = optional_usize(arguments, "limit")?
            .filter(|n| *n > 0)
            .unwrap_or(crate::index_diff::DEFAULT_MAX_ENTRIES);

        // Git is read read-only and fail-soft, off the async runtime since it
        // shells out.
        let git_root = self.project_root.clone();
        let git = match tokio::task::spawn_blocking(move || {
            crate::index_diff::gather_git_facts(&git_root)
        })
        .await
        {
            Ok(facts) => facts,
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::Internal,
                    &format!("git fact-gathering task failed: {err}"),
                    true,
                ));
            }
        };

        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                let state = crate::index_diff::read_index_state(conn)?;
                Ok(success_envelope(crate::index_diff::build_report(
                    &project_root,
                    &state,
                    &git,
                    cap,
                )))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    pub(crate) fn llm_diagnostics_json(&self) -> Value {
        match &self.diagnostics {
            Some(diag) => json!({
                "provider": diag.llm.provider,
                "live": diag.llm.live,
                "allow_live_provider": diag.llm.allow_live_provider,
                "cache_max_age_days": diag.llm.cache_max_age_days,
            }),
            None => Value::Null,
        }
    }

    pub(crate) fn filigree_diagnostics_json(&self) -> Value {
        match &self.diagnostics {
            Some(diag) => json!({
                "enabled": diag.filigree.enabled,
                "configured_url": diag.filigree.configured_url,
                "resolved_url": diag.filigree.resolved_url,
                "resolution_source": diag.filigree.source,
            }),
            None => Value::Null,
        }
    }

    /// ADR-044: report the live read-API endpoint resolved from
    /// `.loomweave/ephemeral.port` (the reference reader; `doctor` reports the
    /// same). Pass `None` config — `project_status` has no static loomweave URL
    /// of its own; this surfaces whether serve is currently publishing.
    pub(crate) fn loomweave_read_api_json(&self) -> Value {
        let resolution =
            loomweave_federation::loomweave_url::resolve_loomweave_url(None, &self.project_root);
        json!({
            "resolved_url": resolution.resolved_url,
            "resolution_source": resolution.source,
        })
    }

    pub(crate) async fn read_issues_for_entities(
        &self,
        entity_id: String,
        include_contained: bool,
    ) -> Result<Option<IssuesForRead>, StorageError> {
        self.readers
            .with_reader(move |conn| {
                let Some(root) = entity_by_id(conn, &entity_id)? else {
                    return Ok(None);
                };
                let mut ids = vec![root.id.clone()];
                let mut entity_cap_truncated = false;
                if include_contained {
                    let contained = contained_entity_ids(conn, &entity_id, 1_000)?;
                    entity_cap_truncated = contained.truncated;
                    ids.extend(contained.entity_ids);
                }
                let mut entities = Vec::with_capacity(ids.len());
                for id in ids {
                    if let Some(entity) = entity_by_id(conn, &id)? {
                        entities.push(entity);
                    }
                }
                // Resolve each entity's `sei` while a reader connection is in
                // scope; `tool_issues_for` consumes this map outside any reader
                // closure (REQ-C-04 / ADR-038).
                let entity_json_by_id: HashMap<String, Value> = entities
                    .iter()
                    .map(|entity| (entity.id.clone(), entity_json(conn, entity)))
                    .collect();
                Ok(Some(IssuesForRead {
                    entities,
                    entity_json_by_id,
                    entity_cap_truncated,
                }))
            })
            .await
    }
}
