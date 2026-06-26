//! On-demand summary + LLM-inferred-edge dispatch (`summary`, `refresh`, budget ledger).
//!
//! Extracted from `lib.rs` (V11-ARCH-04). Methods attach to
//! [`crate::ServerState`] via an inherent `impl` block; `lib.rs` keeps the
//! shared free-function helpers, the tool catalogue, and the JSON-RPC dispatch.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use loomweave_core::{EdgeConfidence, McpErrorCode};
use loomweave_llm::{
    INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput, LEAF_SUMMARY_PROMPT_TEMPLATE_ID,
    LeafSummaryPromptInput, LlmPurpose, LlmRequest, build_inferred_calls_prompt,
    build_leaf_summary_prompt,
};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};

use loomweave_storage::{
    EntityRow, InferredCallEdgeRecord, InferredEdgeCacheEntry, InferredEdgeCacheKey, StorageError,
    SummaryCacheEntry, SummaryCacheKey, WriterCmd, call_edges_from, call_edges_targeting,
    candidate_entities_for_unresolved_sites, entity_by_id, existing_entity_ids,
    guidance_sheet_is_expired, guidance_sheet_matches_entity, inferred_edge_cache_lookup,
    list_guidance_sheets, resolve_entity_ref, summary_cache_lookup,
    unresolved_call_sites_for_caller, unresolved_callers_for_target,
};

use crate::{
    BudgetReservation, EMPTY_GUIDANCE_FINGERPRINT, InferenceLlmState, InferredDispatchFailure,
    InferredDispatchOutcome, InferredDispatchStats, InferredInflightGuard, InferredRead,
    ParamError, ServerState, SummaryLlmState, SummaryRead, SummaryReady, briefing_block_reason,
    entities_json, entity_identity_json, entity_json, inferred_records_from_result,
    inferred_usage_stats, invoke_llm_provider, llm_usage_json, required_str, stale_semantic,
    storage_retryable, structural_summary_json, summary_cache_expired, summary_read_error,
    summary_success_envelope, summary_usage_stats, token_ceiling_envelope, tool_error_envelope,
    unresolved_sites_json, verified_source_excerpt,
};

fn composed_summary_guidance(
    conn: &rusqlite::Connection,
    entity: &EntityRow,
    project_root: &Path,
    now: &str,
) -> Result<String, StorageError> {
    let explicit_sheet_ids: HashSet<String> = {
        let mut stmt =
            conn.prepare("SELECT from_id FROM edges WHERE kind = 'guides' AND to_id = ?1")?;
        let rows = stmt.query_map(rusqlite::params![entity.id], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let canonical_root = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut blocks = Vec::new();
    for sheet in list_guidance_sheets(conn)? {
        if guidance_sheet_is_expired(&sheet, now) {
            continue;
        }
        let matched = explicit_sheet_ids.contains(&sheet.id)
            || guidance_sheet_matches_entity(conn, &sheet, &entity.id, &canonical_root)?;
        if !matched {
            continue;
        }
        let Some(content) = sheet.properties.get("content").and_then(Value::as_str) else {
            continue;
        };
        let content = content.trim();
        if content.is_empty() {
            continue;
        }
        blocks.push(format!("Guidance sheet {}:\n{}", sheet.id, content));
    }
    Ok(blocks.join("\n\n"))
}

fn guidance_fingerprint(guidance_text: &str) -> String {
    if guidance_text.trim().is_empty() {
        EMPTY_GUIDANCE_FINGERPRINT.to_owned()
    } else {
        format!(
            "guidance:{}",
            blake3::hash(guidance_text.as_bytes()).to_hex()
        )
    }
}

impl ServerState {
    pub(crate) async fn tool_summary(
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

        let SummaryRead::Ready(ready) = read else {
            return Ok(summary_read_error(read));
        };

        if let Some(envelope) = self.cached_summary_envelope(&ready, &now).await {
            return Ok(envelope);
        }

        if self.summary_budget_blocked() {
            return Ok(token_ceiling_envelope(
                "LLM session token ceiling has been reached",
            ));
        }

        let Some(summary_llm) = &self.summary_llm else {
            return Ok(tool_error_envelope(
                McpErrorCode::LlmDisabled,
                "LLM summaries are disabled and no fresh cache row is available",
                false,
            ));
        };
        if !summary_llm.config.enabled {
            return Ok(tool_error_envelope(
                McpErrorCode::LlmDisabled,
                "LLM summaries are disabled and no fresh cache row is available",
                false,
            ));
        }

        Ok(self.refresh_summary(*ready, summary_llm, now).await)
    }

    /// Resolve an id-or-SEI to its canonical `entities.id` locator via a single
    /// reader call. Returns `Ok(None)` when nothing alive resolves. Used to
    /// canonicalize a raw arg ONCE at the top of the `ensure_inferred_*`
    /// pre-gated tools so both the inference pass and the reader key on the real
    /// locator rather than re-resolving a SEI each time (clarion-d76e7f7267).
    pub(crate) async fn resolve_to_locator(
        &self,
        id_or_sei: &str,
    ) -> Result<Option<String>, StorageError> {
        let id_or_sei = id_or_sei.to_owned();
        self.readers
            .with_reader(move |conn| {
                Ok(resolve_entity_ref(conn, &id_or_sei)?.map(|entity| entity.id))
            })
            .await
    }

    pub(crate) async fn ensure_inferred_for_target(
        &self,
        target_id: &str,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let target_id = target_id.to_owned();
        let caller_ids = self
            .readers
            .with_reader(move |conn| {
                let Some(target) = entity_by_id(conn, &target_id)? else {
                    return Ok(Vec::new());
                };
                let sites = unresolved_callers_for_target(conn, &target, 50)?;
                let mut seen = std::collections::BTreeSet::new();
                Ok(sites
                    .into_iter()
                    .filter_map(|site| {
                        if seen.insert(site.caller_entity_id.clone()) {
                            Some(site.caller_entity_id)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>())
            })
            .await
            .map_err(|err| InferredDispatchFailure::from_storage(&err))?;

        let mut stats = InferredDispatchStats {
            candidate_callers_considered: u64::try_from(caller_ids.len()).unwrap_or(u64::MAX),
            ..InferredDispatchStats::default()
        };
        for caller_id in caller_ids {
            stats.merge(&self.ensure_inferred_for_caller(&caller_id).await?);
        }
        Ok(stats)
    }

    pub(crate) async fn ensure_inferred_for_caller(
        &self,
        caller_id: &str,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let model_id = self.inferred_edges_model_id();
        let Some(read) = self
            .read_inferred_inputs(caller_id.to_owned(), model_id)
            .await?
        else {
            return Ok(InferredDispatchStats::default());
        };

        if let Some(reason) = briefing_block_reason(&read.caller) {
            tracing::warn!(
                caller_id = %caller_id,
                briefing_blocked = %reason,
                "skipping inferred-edge dispatch for briefing-blocked caller"
            );
            return Ok(InferredDispatchStats::briefing_blocked());
        }

        if let Some(cached) = read.cached.clone() {
            return self.materialize_cached_inferred(read, cached).await;
        }

        if self.summary_budget_blocked() {
            return Err(InferredDispatchFailure::new(
                McpErrorCode::TokenCeilingExceeded,
                "LLM session token ceiling has been reached",
                false,
            ));
        }
        let Some(llm) = self.inference_llm_snapshot() else {
            return Err(InferredDispatchFailure::new(
                McpErrorCode::LlmDisabled,
                "LLM inferred-edge dispatch is disabled and no cache row is available",
                false,
            ));
        };
        if !llm.config.enabled {
            return Err(InferredDispatchFailure::new(
                McpErrorCode::LlmDisabled,
                "LLM inferred-edge dispatch is disabled and no cache row is available",
                false,
            ));
        }

        self.coalesced_inferred_dispatch(read.key.clone(), read, llm)
            .await
    }

    pub(crate) async fn read_inferred_inputs(
        &self,
        caller_id: String,
        model_id: String,
    ) -> Result<Option<InferredRead>, InferredDispatchFailure> {
        self.readers
            .with_reader(move |conn| {
                let Some(caller) = entity_by_id(conn, &caller_id)? else {
                    return Ok(None);
                };
                let Some(content_hash) = caller.content_hash.clone() else {
                    return Ok(None);
                };
                let sites = unresolved_call_sites_for_caller(conn, &caller_id, 100)?;
                if sites.is_empty() {
                    return Ok(None);
                }
                let candidates = candidate_entities_for_unresolved_sites(conn, &sites, 100)?;
                let key = InferredEdgeCacheKey {
                    caller_entity_id: caller.id.clone(),
                    caller_content_hash: content_hash,
                    model_id,
                    prompt_version: INFERRED_CALLS_PROMPT_VERSION.to_owned(),
                };
                let cached = inferred_edge_cache_lookup(conn, &key)?;
                Ok(Some(InferredRead {
                    caller,
                    sites,
                    candidates,
                    key,
                    cached,
                }))
            })
            .await
            .map_err(|err| InferredDispatchFailure::from_storage(&err))
    }

    pub(crate) async fn materialize_cached_inferred(
        &self,
        read: InferredRead,
        mut cached: InferredEdgeCacheEntry,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let Some(llm) = self.inference_llm_snapshot() else {
            return Err(InferredDispatchFailure::new(
                McpErrorCode::LlmDisabled,
                "LLM inferred-edge dispatch is disabled and no writer is available",
                false,
            ));
        };
        let now = (self.clock)();
        cached.last_accessed_at = now;
        let edges = inferred_records_from_result(
            &read,
            &cached.result_json,
            self.max_inferred_edges_per_caller(),
        )?;
        let (edges, dropped) = self.drop_unresolved_inferred_targets(edges).await?;
        let write = self
            .send_writer(&llm.writer, |ack| WriterCmd::InsertInferredEdges {
                cache_entry: Box::new(cached),
                edges,
                ack,
            })
            .await
            .map_err(|err| InferredDispatchFailure::from_storage(&err))?;
        let mut stats = InferredDispatchStats::cache_hit(write);
        stats.unresolved_targets_dropped_total = dropped;
        Ok(stats)
    }

    pub(crate) async fn coalesced_inferred_dispatch(
        &self,
        key: InferredEdgeCacheKey,
        read: InferredRead,
        llm: InferenceLlmState,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let (maybe_rx, leader_sender) = {
            let mut in_flight = self.inferred_inflight.lock().await;
            if let Some(sender) = in_flight.get(&key) {
                (Some(sender.subscribe()), None)
            } else {
                let (sender, _) = broadcast::channel(8);
                in_flight.insert(key.clone(), sender.clone());
                (None, Some(sender))
            }
        };

        if let Some(mut rx) = maybe_rx {
            return match tokio::time::timeout(std::time::Duration::from_secs(60), rx.recv()).await {
                Ok(Ok(outcome)) => {
                    let mut stats = outcome.into_result()?;
                    stats.coalesced_waits_total += 1;
                    Ok(stats)
                }
                Ok(Err(_)) => Err(InferredDispatchFailure::new(
                    McpErrorCode::InferredDispatchCancelled,
                    "inferred dispatch owner ended before broadcasting a result",
                    true,
                )),
                Err(_) => Err(InferredDispatchFailure::new(
                    McpErrorCode::InferredDispatchTimeout,
                    "timed out waiting for in-flight inferred dispatch",
                    true,
                )),
            };
        }

        let guard = InferredInflightGuard::new(
            Arc::clone(&self.inferred_inflight),
            key,
            leader_sender.expect("leader sender is present for non-coalesced dispatch"),
        );
        let outcome =
            InferredDispatchOutcome::from_result(self.perform_inferred_dispatch(read, &llm).await);
        if let Some(sender) = guard.remove().await {
            let _ = sender.send(outcome.clone());
        }
        outcome.into_result()
    }

    pub(crate) async fn perform_inferred_dispatch(
        &self,
        read: InferredRead,
        llm: &InferenceLlmState,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let caller_source_excerpt =
            verified_source_excerpt(&read.caller).map_err(|err| err.to_inferred_failure())?;
        let prompt = build_inferred_calls_prompt(&InferredCallsPromptInput {
            caller_entity_id: read.caller.id.clone(),
            caller_source_excerpt,
            unresolved_call_sites_json: unresolved_sites_json(&read.sites),
            candidate_entities_json: entities_json(&read.candidates),
            max_edges: self.max_inferred_edges_per_caller(),
        });
        let request = LlmRequest {
            purpose: LlmPurpose::InferredEdges,
            model_id: read.key.model_id.clone(),
            prompt_id: prompt.id.to_owned(),
            prompt: prompt.body,
            max_output_tokens: 2048,
        };
        let Some(reservation) = self.reserve_budget(
            llm.provider.estimate_tokens(&request),
            llm.config.session_token_ceiling,
        ) else {
            return Err(InferredDispatchFailure::new(
                McpErrorCode::TokenCeilingExceeded,
                "LLM session token ceiling has been reached",
                false,
            ));
        };
        let response = invoke_llm_provider(Arc::clone(&llm.provider), request)
            .await
            .map_err(|err| {
                InferredDispatchFailure::new(
                    McpErrorCode::LlmProviderError,
                    &err.to_string(),
                    err.retryable(),
                )
            })?;
        if !reservation.commit(
            u64::from(response.total_tokens),
            llm.config.session_token_ceiling,
        ) {
            return Err(InferredDispatchFailure::new(
                McpErrorCode::TokenCeilingExceeded,
                "LLM session token ceiling has been reached",
                false,
            ));
        }
        let edges = match inferred_records_from_result(
            &read,
            &response.output_json,
            self.max_inferred_edges_per_caller(),
        ) {
            Ok(edges) => edges,
            Err(err) if err.code == McpErrorCode::LlmInvalidJson => {
                let message = err.message.clone();
                return Err(err.with_stats(
                    inferred_usage_stats(&response, true),
                    vec![json!({
                        "code": "LMWV-LLM-INVALID-JSON",
                        "message": message,
                        "usage": llm_usage_json(&response)
                    })],
                ));
            }
            Err(err) => return Err(err),
        };
        let (edges, dropped) = self.drop_unresolved_inferred_targets(edges).await?;
        let now = (self.clock)();
        let entry = InferredEdgeCacheEntry {
            key: read.key,
            result_json: response.output_json.clone(),
            cost_usd: response.cost_usd,
            token_count: i64::from(response.total_tokens),
            created_at: now.clone(),
            last_accessed_at: now,
        };
        let write = self
            .send_writer(&llm.writer, |ack| WriterCmd::InsertInferredEdges {
                cache_entry: Box::new(entry.clone()),
                edges,
                ack,
            })
            .await
            .map_err(|err| InferredDispatchFailure::from_storage(&err))?;
        let mut stats = InferredDispatchStats::cache_miss(write, &response);
        stats.unresolved_targets_dropped_total = dropped;
        Ok(stats)
    }

    /// Strip `to_id`s that don't exist in the `entities` table so the
    /// writer-actor's FK-protected INSERT never sees a hallucinated edge
    /// target (clarion-df58379de4). Returns the surviving records and the
    /// count of dropped edges so callers can fold the number into
    /// `InferredDispatchStats`.
    pub(crate) async fn drop_unresolved_inferred_targets(
        &self,
        records: Vec<InferredCallEdgeRecord>,
    ) -> Result<(Vec<InferredCallEdgeRecord>, u64), InferredDispatchFailure> {
        if records.is_empty() {
            return Ok((records, 0));
        }
        let unique_targets: Vec<String> = records
            .iter()
            .map(|record| record.to_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let existing = self
            .readers
            .with_reader({
                let targets = unique_targets.clone();
                move |conn| existing_entity_ids(conn, &targets)
            })
            .await
            .map_err(|err| InferredDispatchFailure::from_storage(&err))?;
        let original_len = records.len();
        let kept: Vec<InferredCallEdgeRecord> = records
            .into_iter()
            .filter(|record| existing.contains(&record.to_id))
            .collect();
        let dropped = u64::try_from(original_len - kept.len()).unwrap_or(0);
        Ok((kept, dropped))
    }

    pub(crate) async fn read_summary_inputs(
        &self,
        entity_id: String,
        summary_model_id: String,
        now: String,
    ) -> Result<SummaryRead, StorageError> {
        let project_root = self.project_root.clone();
        self.readers
            .with_reader(move |conn| {
                let Some(entity) = resolve_entity_ref(conn, &entity_id)? else {
                    return Ok(SummaryRead::EntityNotFound(entity_id));
                };
                if entity.kind == "subsystem" {
                    return Ok(SummaryRead::ScopeDeferred(entity_json(conn, &entity)));
                }
                if let Some(reason) = briefing_block_reason(&entity) {
                    // Deliberate exception to the `entity_json` identity gate
                    // (clarion-307668e2be): the caller named this exact id and
                    // needs the remediation echo ("fix the secret at <path>"), so
                    // build identity via the conn-free core that bypasses the
                    // redaction. The caller cannot *discover* what it already named.
                    return Ok(SummaryRead::BriefingBlocked(
                        entity_identity_json(&entity),
                        reason,
                    ));
                }
                let Some(content_hash) = entity.content_hash.clone() else {
                    return Ok(SummaryRead::MissingContentHash(entity.id));
                };
                let guidance_text = composed_summary_guidance(conn, &entity, &project_root, &now)?;
                let guidance_fingerprint = guidance_fingerprint(&guidance_text);
                let key = SummaryCacheKey {
                    entity_id: entity.id.clone(),
                    content_hash,
                    prompt_template_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                    model_tier: summary_model_id,
                    guidance_fingerprint,
                };
                let cached = summary_cache_lookup(conn, &key)?;
                let caller_count = i64::try_from(
                    call_edges_targeting(conn, &entity.id, EdgeConfidence::Ambiguous)?.len(),
                )
                .unwrap_or(i64::MAX);
                let fan_out = i64::try_from(
                    call_edges_from(conn, &entity.id, EdgeConfidence::Ambiguous)?.len(),
                )
                .unwrap_or(i64::MAX);
                let entity_payload = entity_json(conn, &entity);
                Ok(SummaryRead::Ready(Box::new(SummaryReady {
                    entity,
                    entity_json: entity_payload,
                    key,
                    cached,
                    guidance_text,
                    caller_count,
                    fan_out,
                })))
            })
            .await
    }

    pub(crate) async fn cached_summary_envelope(
        &self,
        ready: &SummaryReady,
        now: &str,
    ) -> Option<Value> {
        let cached = ready.cached.as_ref()?;
        if summary_cache_expired(&cached.created_at, now, self.summary_cache_max_age_days()) {
            return None;
        }
        if let Some(summary_llm) = &self.summary_llm
            && let Err(err) = self
                .send_writer(&summary_llm.writer, |ack| WriterCmd::TouchSummaryCache {
                    key: ready.key.clone(),
                    last_accessed_at: now.to_owned(),
                    ack,
                })
                .await
        {
            return Some(tool_error_envelope(
                McpErrorCode::StorageError,
                &err.to_string(),
                storage_retryable(&err),
            ));
        }
        Some(summary_success_envelope(
            &ready.entity_json,
            cached,
            true,
            stale_semantic(cached, ready.caller_count, ready.fan_out),
            None,
            json!({"summary_cache_hits_total": 1}),
        ))
    }

    pub(crate) async fn refresh_summary(
        &self,
        ready: SummaryReady,
        summary_llm: &SummaryLlmState,
        now: String,
    ) -> Value {
        let model_id = self.summary_model_id();
        let source_excerpt = match verified_source_excerpt(&ready.entity) {
            Ok(excerpt) => excerpt,
            Err(err) => return err.to_envelope(),
        };
        let prompt = build_leaf_summary_prompt(&LeafSummaryPromptInput {
            entity_id: ready.entity.id.clone(),
            kind: ready.entity.kind.clone(),
            name: ready.entity.name.clone(),
            guidance: ready.guidance_text.clone(),
            source_excerpt: source_excerpt.clone(),
        });
        let request = LlmRequest {
            purpose: LlmPurpose::Summary,
            model_id: model_id.clone(),
            prompt_id: prompt.id.to_owned(),
            prompt: prompt.body,
            max_output_tokens: 512,
        };
        let Some(reservation) = self.reserve_budget(
            summary_llm.provider.estimate_tokens(&request),
            summary_llm.config.session_token_ceiling,
        ) else {
            return token_ceiling_envelope("LLM session token ceiling has been reached");
        };
        let response = match invoke_llm_provider(Arc::clone(&summary_llm.provider), request).await {
            Ok(response) => response,
            Err(err) => {
                return tool_error_envelope(
                    McpErrorCode::LlmProviderError,
                    &err.to_string(),
                    err.retryable(),
                );
            }
        };

        if !reservation.commit(
            u64::from(response.total_tokens),
            summary_llm.config.session_token_ceiling,
        ) {
            return token_ceiling_envelope("LLM session token ceiling has been reached");
        }

        if serde_json::from_str::<Value>(&response.output_json).is_err() {
            // The provider returned non-JSON — a deterministic failure for this
            // input. Rather than bill the caller for an error and force the same
            // paid failure on every retry, fall back to a structural summary
            // built from the entity's own source and cache it, so the next
            // request is a free cache hit (clarion-ed246ca3aa).
            let mut stats_delta = summary_usage_stats(&response, true);
            if let Some(object) = stats_delta.as_object_mut() {
                object.insert("summary_structural_fallback_total".to_owned(), json!(1));
            }
            let cached_input_tokens = i64::from(response.cached_input_tokens);
            let entry = SummaryCacheEntry {
                key: ready.key,
                summary_json: structural_summary_json(&ready.entity, &source_excerpt),
                cost_usd: response.cost_usd,
                tokens_input: i64::from(response.input_tokens),
                tokens_output: i64::from(response.output_tokens),
                caller_count: ready.caller_count,
                fan_out: ready.fan_out,
                stale_semantic: false,
                created_at: now.clone(),
                last_accessed_at: now,
            };
            if let Err(err) = self
                .send_writer(&summary_llm.writer, |ack| WriterCmd::UpsertSummaryCache {
                    entry: Box::new(entry.clone()),
                    ack,
                })
                .await
            {
                return tool_error_envelope(
                    McpErrorCode::StorageError,
                    &err.to_string(),
                    storage_retryable(&err),
                );
            }
            return summary_success_envelope(
                &ready.entity_json,
                &entry,
                false,
                false,
                Some(cached_input_tokens),
                stats_delta,
            );
        }

        let cached_input_tokens = i64::from(response.cached_input_tokens);
        let stats_delta = summary_usage_stats(&response, false);
        let entry = SummaryCacheEntry {
            key: ready.key,
            summary_json: response.output_json,
            cost_usd: response.cost_usd,
            tokens_input: i64::from(response.input_tokens),
            tokens_output: i64::from(response.output_tokens),
            caller_count: ready.caller_count,
            fan_out: ready.fan_out,
            stale_semantic: false,
            created_at: now.clone(),
            last_accessed_at: now,
        };
        if let Err(err) = self
            .send_writer(&summary_llm.writer, |ack| WriterCmd::UpsertSummaryCache {
                entry: Box::new(entry.clone()),
                ack,
            })
            .await
        {
            return tool_error_envelope(
                McpErrorCode::StorageError,
                &err.to_string(),
                storage_retryable(&err),
            );
        }

        summary_success_envelope(
            &ready.entity_json,
            &entry,
            false,
            false,
            Some(cached_input_tokens),
            stats_delta,
        )
    }

    pub(crate) async fn send_writer<T>(
        &self,
        writer: &mpsc::Sender<WriterCmd>,
        build: impl FnOnce(oneshot::Sender<Result<T, StorageError>>) -> WriterCmd,
    ) -> Result<T, StorageError>
    where
        T: Send + 'static,
    {
        let (ack_tx, ack_rx) = oneshot::channel();
        writer
            .send(build(ack_tx))
            .await
            .map_err(|_| StorageError::WriterGone)?;
        tokio::time::timeout(std::time::Duration::from_secs(30), ack_rx)
            .await
            .map_err(|_| StorageError::WriterNoResponse)?
            .map_err(|_| StorageError::WriterNoResponse)?
    }

    pub(crate) fn summary_budget_blocked(&self) -> bool {
        self.budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .blocked
    }

    pub(crate) fn reserve_budget(
        &self,
        estimate_tokens: u64,
        ceiling_tokens: u64,
    ) -> Option<BudgetReservation> {
        let mut budget = self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if budget.blocked
            || budget
                .spent_tokens
                .saturating_add(budget.reserved_tokens)
                .saturating_add(estimate_tokens)
                > ceiling_tokens
        {
            budget.blocked = true;
            return None;
        }
        budget.reserved_tokens = budget.reserved_tokens.saturating_add(estimate_tokens);
        Some(BudgetReservation {
            budget: Arc::clone(&self.budget),
            amount_tokens: estimate_tokens,
            active: true,
        })
    }

    pub(crate) fn inference_llm_snapshot(&self) -> Option<InferenceLlmState> {
        self.summary_llm.as_ref().map(|llm| InferenceLlmState {
            writer: llm.writer.clone(),
            config: llm.config.clone(),
            provider: Arc::clone(&llm.provider),
        })
    }

    pub(crate) fn summary_cache_max_age_days(&self) -> u32 {
        self.summary_llm
            .as_ref()
            .map_or(180, |summary| summary.config.cache_max_age_days)
    }

    pub(crate) fn summary_model_id(&self) -> String {
        self.summary_llm.as_ref().map_or_else(
            || "anthropic/claude-sonnet-4.6".to_owned(),
            |summary| {
                summary
                    .provider
                    .tier_to_model("summary")
                    .unwrap_or(&summary.config.model_id)
                    .to_owned()
            },
        )
    }

    pub(crate) fn inferred_edges_model_id(&self) -> String {
        self.summary_llm.as_ref().map_or_else(
            || "anthropic/claude-sonnet-4.6".to_owned(),
            |summary| {
                summary
                    .provider
                    .tier_to_model("inferred_edges")
                    .unwrap_or(&summary.config.model_id)
                    .to_owned()
            },
        )
    }

    pub(crate) fn max_inferred_edges_per_caller(&self) -> usize {
        self.summary_llm.as_ref().map_or(8, |summary| {
            usize::try_from(summary.config.max_inferred_edges_per_caller).unwrap_or(8)
        })
    }
}
