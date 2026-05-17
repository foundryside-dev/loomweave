//! MCP storage-backed tool tests.

use std::sync::{Arc, Mutex};

use clarion_core::{
    CachingModel, INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput,
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID, LeafSummaryPromptInput, LlmProvider, LlmProviderError,
    LlmPurpose, LlmRequest, LlmResponse, OpenRouterProvider, OpenRouterProviderConfig, Recording,
    RecordingProvider, build_inferred_calls_prompt, build_leaf_summary_prompt,
};
use clarion_mcp::{
    ServerState,
    config::{LlmConfig, LlmProviderKind},
    filigree::{
        EntityAssociation, EntityAssociationsResponse, FiligreeClientError, FiligreeLookup,
    },
};
use clarion_storage::{
    ReaderPool, SummaryCacheEntry, SummaryCacheKey, Writer, pragma, schema, upsert_summary_cache,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn open_project() -> (tempfile::TempDir, std::path::PathBuf) {
    let project = tempfile::tempdir().expect("temp project");
    let clarion_dir = project.path().join(".clarion");
    std::fs::create_dir(&clarion_dir).expect("create .clarion");
    let db_path = clarion_dir.join("clarion.db");
    let mut conn = Connection::open(&db_path).expect("open sqlite");
    pragma::apply_write_pragmas(&conn).expect("write pragmas");
    schema::apply_migrations(&mut conn).expect("apply migrations");
    seed_graph(&conn, project.path());
    drop(conn);
    (project, db_path)
}

fn seed_graph(conn: &Connection, project_root: &std::path::Path) {
    let source_path = project_root.join("demo.py");
    std::fs::write(
        &source_path,
        "def entry():\n    return mid()\n\ndef mid():\n    return target()\n\ndef target():\n    return 1\n",
    )
    .expect("write demo source");

    insert_entity(
        conn,
        "python:module:demo",
        "module",
        &source_path,
        Some((1, 8)),
        None,
    );
    insert_entity(
        conn,
        "python:function:demo.entry",
        "function",
        &source_path,
        Some((1, 2)),
        Some("python:module:demo"),
    );
    insert_entity(
        conn,
        "python:function:demo.mid",
        "function",
        &source_path,
        Some((4, 5)),
        Some("python:module:demo"),
    );
    insert_entity(
        conn,
        "python:function:demo.target",
        "function",
        &source_path,
        Some((7, 8)),
        Some("python:module:demo"),
    );
    insert_entity(
        conn,
        "python:function:demo.alt_target",
        "function",
        &source_path,
        Some((7, 8)),
        Some("python:module:demo"),
    );
    insert_edge(
        conn,
        "contains",
        "python:module:demo",
        "python:function:demo.entry",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "contains",
        "python:module:demo",
        "python:function:demo.mid",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "contains",
        "python:module:demo",
        "python:function:demo.target",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "contains",
        "python:module:demo",
        "python:function:demo.alt_target",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "calls",
        "python:function:demo.entry",
        "python:function:demo.mid",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "calls",
        "python:function:demo.mid",
        "python:function:demo.target",
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "calls",
        "python:function:demo.entry",
        "python:function:demo.target",
        "ambiguous",
        Some(json!({"candidates": ["python:function:demo.alt_target"]})),
    );
    insert_edge(
        conn,
        "references",
        "python:function:demo.entry",
        "python:function:demo.target",
        "resolved",
        None,
    );
}

fn insert_entity(
    conn: &Connection,
    id: &str,
    kind: &str,
    source_path: &std::path::Path,
    range: Option<(i64, i64)>,
    parent_id: Option<&str>,
) {
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            ?1, 'python', ?2, ?1, ?1, ?3, ?4, ?5, ?6, '{}', ?7,
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        params![
            id,
            kind,
            parent_id,
            source_path.display().to_string(),
            range.map(|(start, _)| start),
            range.map(|(_, end)| end),
            format!("hash-{id}"),
        ],
    )
    .expect("insert entity");
}

fn insert_edge(
    conn: &Connection,
    kind: &str,
    from_id: &str,
    to_id: &str,
    confidence: &str,
    properties: Option<Value>,
) {
    let anchored = matches!(kind, "calls" | "references");
    conn.execute(
        "INSERT INTO edges (
            kind, from_id, to_id, confidence, properties, source_byte_start, source_byte_end
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            kind,
            from_id,
            to_id,
            confidence,
            properties.map(|value| value.to_string()),
            anchored.then_some(10),
            anchored.then_some(20),
        ],
    )
    .expect("insert edge");
}

fn insert_unresolved_call_site(conn: &Connection, caller_id: &str, site_key: &str, expr: &str) {
    conn.execute(
        "INSERT INTO entity_unresolved_call_sites (
            caller_entity_id, caller_content_hash, site_key, site_ordinal,
            source_file_id, source_byte_start, source_byte_end, callee_expr, created_at
         ) VALUES (?1, ?2, ?3, 0, 'python:module:demo', 30, 37, ?4, '2026-05-17T00:00:00.000Z')",
        params![caller_id, format!("hash-{caller_id}"), site_key, expr],
    )
    .expect("insert unresolved call site");
}

fn state_for(project_root: &std::path::Path, db_path: &std::path::Path) -> ServerState {
    let pool = ReaderPool::open(db_path, 2).expect("reader pool");
    ServerState::new(project_root.to_path_buf(), pool)
}

fn llm_config() -> LlmConfig {
    LlmConfig {
        enabled: true,
        provider: LlmProviderKind::Recording,
        ..LlmConfig::default()
    }
}

fn state_for_summary(
    project_root: &std::path::Path,
    db_path: &std::path::Path,
    writer: &Writer,
    provider: Arc<dyn LlmProvider>,
    config: LlmConfig,
) -> ServerState {
    let pool = ReaderPool::open(db_path, 2).expect("reader pool");
    ServerState::new(project_root.to_path_buf(), pool)
        .with_summary_llm(writer.sender(), config, provider)
        .with_clock(|| "2026-05-17T00:00:02.000Z".to_owned())
}

fn state_for_filigree(
    project_root: &std::path::Path,
    db_path: &std::path::Path,
    client: Arc<dyn FiligreeLookup>,
) -> ServerState {
    let pool = ReaderPool::open(db_path, 2).expect("reader pool");
    ServerState::new(project_root.to_path_buf(), pool).with_filigree_client(client)
}

fn expected_summary_request(project_root: &std::path::Path, entity_id: &str) -> LlmRequest {
    let source_excerpt = expected_source_excerpt(project_root, entity_id);
    let prompt = build_leaf_summary_prompt(&LeafSummaryPromptInput {
        entity_id: entity_id.to_owned(),
        kind: "function".to_owned(),
        name: entity_id.to_owned(),
        source_excerpt,
    });
    LlmRequest {
        purpose: LlmPurpose::Summary,
        model_id: "anthropic/claude-sonnet-4.6".to_owned(),
        prompt_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
        prompt: prompt.body,
        max_output_tokens: 512,
    }
}

fn summary_recording(project_root: &std::path::Path, entity_id: &str) -> Arc<RecordingProvider> {
    Arc::new(RecordingProvider::from_recordings(vec![Recording {
        request: expected_summary_request(project_root, entity_id),
        response: LlmResponse {
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            output_json: r#"{"purpose":"cached demo"}"#.to_owned(),
            input_tokens: 120,
            output_tokens: 24,
            total_tokens: 144,
            cost_usd: 0.0,
        },
    }]))
}

fn expected_inferred_request(
    project_root: &std::path::Path,
    caller_id: &str,
    site_key: &str,
    callee_expr: &str,
    target_id: &str,
) -> LlmRequest {
    let source_excerpt = expected_source_excerpt(project_root, caller_id);
    let unresolved_call_sites_json = serde_json::to_string(&vec![json!({
        "caller_entity_id": caller_id,
        "caller_content_hash": format!("hash-{caller_id}"),
        "site_key": site_key,
        "site_ordinal": 0,
        "source_file_id": "python:module:demo",
        "source_byte_start": 30,
        "source_byte_end": 37,
        "callee_expr": callee_expr
    })])
    .unwrap();
    let source_file_path = project_root.join("demo.py").display().to_string();
    let candidate_entities_json = serde_json::to_string(&vec![json!({
        "id": target_id,
        "kind": "function",
        "name": target_id,
        "short_name": target_id,
        "source_file_path": source_file_path,
        "source_line_start": 9,
        "source_line_end": 10,
        "content_hash": format!("hash-{target_id}")
    })])
    .unwrap();
    let prompt = build_inferred_calls_prompt(&InferredCallsPromptInput {
        caller_entity_id: caller_id.to_owned(),
        caller_source_excerpt: source_excerpt,
        unresolved_call_sites_json,
        candidate_entities_json,
        max_edges: 8,
    });
    LlmRequest {
        purpose: LlmPurpose::InferredEdges,
        model_id: "anthropic/claude-sonnet-4.6".to_owned(),
        prompt_id: INFERRED_CALLS_PROMPT_VERSION.to_owned(),
        prompt: prompt.body,
        max_output_tokens: 2048,
    }
}

fn expected_source_excerpt(project_root: &std::path::Path, entity_id: &str) -> String {
    let source = std::fs::read_to_string(project_root.join("demo.py")).unwrap();
    let Some((start_line, end_line)) = expected_line_range(entity_id) else {
        return source;
    };
    let start = usize::try_from(start_line - 1).unwrap();
    let end = usize::try_from(end_line).unwrap();
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    if start >= lines.len() {
        return source;
    }
    lines[start..end.min(lines.len())].concat()
}

fn expected_line_range(entity_id: &str) -> Option<(i64, i64)> {
    match entity_id {
        "python:module:demo" => Some((1, 8)),
        "python:function:demo.entry" => Some((1, 2)),
        "python:function:demo.mid" => Some((4, 5)),
        "python:function:demo.target" | "python:function:demo.alt_target" => Some((7, 8)),
        "python:function:demo.dynamic" => Some((9, 10)),
        _ => None,
    }
}

fn inferred_recording(
    project_root: &std::path::Path,
    caller_id: &str,
    site_key: &str,
    callee_expr: &str,
    target_id: &str,
) -> Arc<RecordingProvider> {
    Arc::new(RecordingProvider::from_recordings(vec![Recording {
        request: expected_inferred_request(
            project_root,
            caller_id,
            site_key,
            callee_expr,
            target_id,
        ),
        response: LlmResponse {
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            output_json: format!(
                r#"{{"edges":[{{"site_key":"{site_key}","target_id":"{target_id}","confidence":0.91,"rationale":"name match"}}]}}"#
            ),
            input_tokens: 100,
            output_tokens: 20,
            total_tokens: 120,
            cost_usd: 0.0,
        },
    }]))
}

#[derive(Debug)]
struct AnyInferredProvider {
    invocations: Mutex<Vec<LlmRequest>>,
    output_json: String,
    delay_ms: u64,
}

impl AnyInferredProvider {
    fn new(output_json: &str) -> Self {
        Self {
            invocations: Mutex::new(Vec::new()),
            output_json: output_json.to_owned(),
            delay_ms: 0,
        }
    }

    fn new_slow(output_json: &str, delay_ms: u64) -> Self {
        Self {
            invocations: Mutex::new(Vec::new()),
            output_json: output_json.to_owned(),
            delay_ms,
        }
    }

    fn invocations(&self) -> Vec<LlmRequest> {
        self.invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[derive(Debug)]
struct AnySummaryProvider {
    invocations: Mutex<Vec<LlmRequest>>,
    output_json: String,
    delay_ms: u64,
    estimate_tokens: u64,
    total_tokens: u32,
    cost_usd: f64,
}

impl AnySummaryProvider {
    fn new_slow(delay_ms: u64, estimate_tokens: u64, total_tokens: u32) -> Self {
        Self {
            invocations: Mutex::new(Vec::new()),
            output_json: r#"{"purpose":"concurrent"}"#.to_owned(),
            delay_ms,
            estimate_tokens,
            total_tokens,
            cost_usd: 0.0,
        }
    }

    fn new_output(output_json: &str, total_tokens: u32, cost_usd: f64) -> Self {
        Self {
            invocations: Mutex::new(Vec::new()),
            output_json: output_json.to_owned(),
            delay_ms: 0,
            estimate_tokens: 0,
            total_tokens,
            cost_usd,
        }
    }

    fn invocations(&self) -> Vec<LlmRequest> {
        self.invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl LlmProvider for AnySummaryProvider {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
        self.invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        if self.delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
        }
        Ok(LlmResponse {
            model_id: request.model_id,
            output_json: self.output_json.clone(),
            input_tokens: 100,
            output_tokens: 20,
            total_tokens: self.total_tokens,
            cost_usd: self.cost_usd,
        })
    }

    fn estimate_tokens(&self, _request: &LlmRequest) -> u64 {
        self.estimate_tokens
    }

    fn tier_to_model(&self, _tier: &str) -> Option<&str> {
        None
    }

    fn caching_model(&self) -> CachingModel {
        CachingModel::OpenAiChatCompletions
    }
}

impl LlmProvider for AnyInferredProvider {
    fn name(&self) -> &'static str {
        "recording"
    }

    fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
        self.invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        if self.delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(self.delay_ms));
        }
        Ok(LlmResponse {
            model_id: request.model_id,
            output_json: self.output_json.clone(),
            input_tokens: 100,
            output_tokens: 20,
            total_tokens: 120,
            cost_usd: 0.0,
        })
    }

    fn estimate_tokens(&self, _request: &LlmRequest) -> u64 {
        0
    }

    fn tier_to_model(&self, _tier: &str) -> Option<&str> {
        None
    }

    fn caching_model(&self) -> CachingModel {
        CachingModel::OpenAiChatCompletions
    }
}

#[derive(Debug, Default)]
struct FakeFiligreeClient {
    responses: Mutex<std::collections::HashMap<String, EntityAssociationsResponse>>,
    calls: Mutex<Vec<String>>,
}

impl FakeFiligreeClient {
    fn with_response(mut self, entity_id: &str, associations: Vec<EntityAssociation>) -> Self {
        self.responses.get_mut().unwrap().insert(
            entity_id.to_owned(),
            EntityAssociationsResponse { associations },
        );
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl FiligreeLookup for FakeFiligreeClient {
    fn associations_for(
        &self,
        entity_id: &str,
    ) -> Result<EntityAssociationsResponse, FiligreeClientError> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entity_id.to_owned());
        Ok(self
            .responses
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(entity_id)
            .cloned()
            .unwrap_or_else(|| EntityAssociationsResponse {
                associations: Vec::new(),
            }))
    }
}

fn association(issue_id: &str, entity_id: &str, content_hash: &str) -> EntityAssociation {
    EntityAssociation {
        issue_id: issue_id.to_owned(),
        clarion_entity_id: entity_id.to_owned(),
        content_hash_at_attach: content_hash.to_owned(),
        attached_at: "2026-05-17T00:00:00.000Z".to_owned(),
        attached_by: "codex".to_owned(),
    }
}

async fn call_tool(state: &ServerState, name: &str, arguments: Value) -> Value {
    let response = state
        .handle_json_rpc(&json!({
            "jsonrpc": "2.0",
            "id": "tool-test",
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], "tool-test");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool content text");
    serde_json::from_str(text).expect("tool envelope JSON")
}

#[tokio::test]
async fn issues_for_returns_unavailable_when_filigree_disabled() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["available"], false);
    assert_eq!(envelope["result"]["reason"], "filigree-disabled");
}

#[tokio::test]
async fn issues_for_includes_contained_entities_and_flags_drift() {
    let (project, db_path) = open_project();
    let client = Arc::new(
        FakeFiligreeClient::default()
            .with_response(
                "python:function:demo.entry",
                vec![association(
                    "filigree-fresh",
                    "python:function:demo.entry",
                    "hash-python:function:demo.entry",
                )],
            )
            .with_response(
                "python:function:demo.mid",
                vec![association(
                    "filigree-drifted",
                    "python:function:demo.mid",
                    "old-hash",
                )],
            ),
    );
    let state = state_for_filigree(project.path(), &db_path, client.clone());

    let envelope = call_tool(&state, "issues_for", json!({"id": "python:module:demo"})).await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["available"], true);
    assert_eq!(
        envelope["result"]["matched"][0]["issue_id"],
        "filigree-fresh"
    );
    assert_eq!(
        envelope["result"]["drifted"][0]["issue_id"],
        "filigree-drifted"
    );
    assert!(
        client
            .calls()
            .contains(&"python:function:demo.entry".to_owned())
    );
    assert!(
        client
            .calls()
            .contains(&"python:function:demo.mid".to_owned())
    );
}

#[tokio::test]
async fn issues_for_respects_include_contained_false() {
    let (project, db_path) = open_project();
    let client = Arc::new(
        FakeFiligreeClient::default()
            .with_response(
                "python:module:demo",
                vec![association(
                    "filigree-module",
                    "python:module:demo",
                    "hash-python:module:demo",
                )],
            )
            .with_response(
                "python:function:demo.entry",
                vec![association(
                    "filigree-entry",
                    "python:function:demo.entry",
                    "hash-python:function:demo.entry",
                )],
            ),
    );
    let state = state_for_filigree(project.path(), &db_path, client.clone());

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:module:demo", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(
        envelope["result"]["matched"][0]["issue_id"],
        "filigree-module"
    );
    assert_eq!(client.calls(), vec!["python:module:demo".to_owned()]);
}

#[tokio::test]
async fn issues_for_truncates_at_issue_cap() {
    let (project, db_path) = open_project();
    let associations = (0..101)
        .map(|idx| {
            association(
                &format!("filigree-{idx:03}"),
                "python:function:demo.entry",
                "hash-python:function:demo.entry",
            )
        })
        .collect();
    let client = Arc::new(
        FakeFiligreeClient::default().with_response("python:function:demo.entry", associations),
    );
    let state = state_for_filigree(project.path(), &db_path, client);

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.entry", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["truncated"], true);
    assert_eq!(envelope["truncation_reason"], "issue-cap");
    assert_eq!(envelope["result"]["matched"].as_array().unwrap().len(), 100);
}

#[tokio::test]
async fn issues_for_stops_filigree_calls_after_issue_cap() {
    let (project, db_path) = open_project();
    let associations = (0..101)
        .map(|idx| {
            association(
                &format!("filigree-{idx:03}"),
                "python:module:demo",
                "hash-python:module:demo",
            )
        })
        .collect();
    let client = Arc::new(
        FakeFiligreeClient::default()
            .with_response("python:module:demo", associations)
            .with_response(
                "python:function:demo.entry",
                vec![association(
                    "filigree-entry",
                    "python:function:demo.entry",
                    "hash-python:function:demo.entry",
                )],
            ),
    );
    let state = state_for_filigree(project.path(), &db_path, client.clone());

    let envelope = call_tool(&state, "issues_for", json!({"id": "python:module:demo"})).await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["truncated"], true);
    assert_eq!(envelope["truncation_reason"], "issue-cap");
    assert_eq!(client.calls(), vec!["python:module:demo".to_owned()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_returns_disabled_when_cache_empty_and_llm_off() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "llm-disabled");
    assert_eq!(envelope["result"], Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_cold_miss_records_provider_response_then_hits_cache() {
    let (project, db_path) = open_project();
    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = summary_recording(project.path(), "python:function:demo.entry");
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let cold = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(cold["ok"], true);
    assert_eq!(cold["result"]["available"], true);
    assert_eq!(cold["result"]["summary"]["purpose"], "cached demo");
    assert_eq!(cold["result"]["cache"]["hit"], false);
    assert_eq!(cold["result"]["cache"]["stale_semantic"], false);
    assert_eq!(cold["stats_delta"]["summary_cache_misses_total"], 1);
    assert_eq!(provider.invocations().len(), 1);

    let warm = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(warm["ok"], true);
    assert_eq!(warm["result"]["cache"]["hit"], true);
    assert_eq!(warm["result"]["summary"]["purpose"], "cached demo");
    assert_eq!(warm["stats_delta"]["summary_cache_hits_total"], 1);
    assert_eq!(provider.invocations().len(), 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_invalid_json_preserves_usage_accounting() {
    let (project, db_path) = open_project();
    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnySummaryProvider::new_output("not-json", 120, 0.012));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let envelope = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "llm-invalid-json");
    assert_eq!(envelope["stats_delta"]["summary_cache_misses_total"], 1);
    assert_eq!(envelope["stats_delta"]["summary_tokens_input"], 100);
    assert_eq!(envelope["stats_delta"]["summary_tokens_output"], 20);
    assert_eq!(envelope["stats_delta"]["summary_tokens_total"], 120);
    assert_eq!(envelope["stats_delta"]["summary_cost_usd"], 0.012);
    assert_eq!(envelope["stats_delta"]["llm_invalid_json_total"], 1);
    assert_eq!(provider.invocations().len(), 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_openrouter_provider_runs_outside_async_runtime() {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    let (project, db_path) = open_project();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test OpenRouter");
    let addr = listener.local_addr().expect("test OpenRouter addr");
    let http = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept OpenRouter request");
        let mut request = [0_u8; 8192];
        let read = stream.read(&mut request).expect("read OpenRouter request");
        let request = String::from_utf8_lossy(&request[..read]);
        assert!(request.contains(r#""response_format":{"json_schema":{"name":"clarion_summary""#));
        let body = r#"{
            "id": "gen-01",
            "object": "chat.completion",
            "created": 1779000000,
            "model": "anthropic/claude-sonnet-4.6",
            "choices": [
                {
                    "finish_reason": "stop",
                    "native_finish_reason": "stop",
                    "message": {
                        "role": "assistant",
                        "content": "{\"purpose\":\"demo\",\"behavior\":\"returns mid\",\"relationships\":\"calls mid\",\"risks\":\"\"}"
                    }
                }
            ],
            "usage": {"prompt_tokens": 100, "completion_tokens": 20, "total_tokens": 120}
        }"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write OpenRouter response");
    });
    let provider = Arc::new(
        OpenRouterProvider::from_config(OpenRouterProviderConfig {
            api_key: Some("secret".to_owned()),
            allow_live_provider: true,
            model_id: "anthropic/claude-sonnet-4.6".to_owned(),
            endpoint_url: format!("http://{addr}/api/v1"),
            referer: "https://github.com/qacona/clarion".to_owned(),
            title: "Clarion Test".to_owned(),
        })
        .expect("OpenRouter provider"),
    );
    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let state = state_for_summary(project.path(), &db_path, &writer, provider, llm_config());

    let envelope = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["summary"]["purpose"], "demo");
    assert_eq!(envelope["stats_delta"]["summary_tokens_total"], 120);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
    http.join().expect("OpenRouter server thread");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_prompt_uses_entity_source_range_not_whole_file() {
    let (project, db_path) = open_project();
    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnySummaryProvider::new_slow(0, 0, 120));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let envelope = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    let invocation = provider
        .invocations()
        .into_iter()
        .next()
        .expect("summary provider invocation");
    assert!(invocation.prompt.contains("def entry():"));
    assert!(invocation.prompt.contains("return mid()"));
    assert!(
        !invocation.prompt.contains("def mid():"),
        "summary prompt leaked neighboring function source: {}",
        invocation.prompt
    );

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_cache_hit_reports_stale_semantic_when_graph_counts_drift() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    upsert_summary_cache(
        &conn,
        &SummaryCacheEntry {
            key: SummaryCacheKey {
                entity_id: "python:function:demo.entry".to_owned(),
                content_hash: "hash-python:function:demo.entry".to_owned(),
                prompt_template_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                model_tier: "anthropic/claude-sonnet-4.6".to_owned(),
                guidance_fingerprint: "guidance-empty".to_owned(),
            },
            summary_json: r#"{"purpose":"old"}"#.to_owned(),
            cost_usd: 0.001,
            tokens_input: 100,
            tokens_output: 20,
            caller_count: 0,
            fan_out: 0,
            stale_semantic: false,
            created_at: "2026-05-17T00:00:00.000Z".to_owned(),
            last_accessed_at: "2026-05-17T00:00:00.000Z".to_owned(),
        },
    )
    .unwrap();
    drop(conn);
    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(RecordingProvider::from_recordings(Vec::new()));
    let state = state_for_summary(project.path(), &db_path, &writer, provider, llm_config());

    let envelope = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["cache"]["hit"], true);
    assert_eq!(envelope["result"]["cache"]["stale_semantic"], true);
    assert_eq!(envelope["stats_delta"]["summary_cache_hits_total"], 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_expired_cache_row_is_refreshed_by_recording_provider() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    upsert_summary_cache(
        &conn,
        &SummaryCacheEntry {
            key: SummaryCacheKey {
                entity_id: "python:function:demo.entry".to_owned(),
                content_hash: "hash-python:function:demo.entry".to_owned(),
                prompt_template_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                model_tier: "anthropic/claude-sonnet-4.6".to_owned(),
                guidance_fingerprint: "guidance-empty".to_owned(),
            },
            summary_json: r#"{"purpose":"old"}"#.to_owned(),
            cost_usd: 0.001,
            tokens_input: 100,
            tokens_output: 20,
            caller_count: 0,
            fan_out: 2,
            stale_semantic: false,
            created_at: "2026-05-01T00:00:00.000Z".to_owned(),
            last_accessed_at: "2026-05-01T00:00:00.000Z".to_owned(),
        },
    )
    .unwrap();
    drop(conn);
    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = summary_recording(project.path(), "python:function:demo.entry");
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        LlmConfig {
            cache_max_age_days: 1,
            ..llm_config()
        },
    );

    let envelope = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["cache"]["hit"], false);
    assert_eq!(envelope["result"]["summary"]["purpose"], "cached demo");
    assert_eq!(envelope["stats_delta"]["summary_cache_misses_total"], 1);
    assert_eq!(provider.invocations().len(), 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_token_ceiling_blocks_session_after_expensive_cold_call() {
    let (project, db_path) = open_project();
    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = summary_recording(project.path(), "python:function:demo.entry");
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        LlmConfig {
            session_token_ceiling: 100,
            ..llm_config()
        },
    );

    let first = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(first["ok"], false);
    assert_eq!(first["error"]["code"], "token-ceiling-exceeded");
    assert_eq!(first["stats_delta"]["token_ceiling_exceeded_total"], 1);
    assert_eq!(
        first["diagnostics"][0]["code"],
        "CLA-LLM-TOKEN-CEILING-EXCEEDED"
    );
    assert_eq!(provider.invocations().len(), 1);

    let second = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(second["ok"], false);
    assert_eq!(second["error"]["code"], "token-ceiling-exceeded");
    assert_eq!(second["stats_delta"]["token_ceiling_exceeded_total"], 1);
    assert_eq!(provider.invocations().len(), 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_token_ceiling_reserves_concurrent_cold_misses() {
    let (project, db_path) = open_project();
    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnySummaryProvider::new_slow(100, 120, 120));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        LlmConfig {
            session_token_ceiling: 150,
            ..llm_config()
        },
    );

    let first = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    );
    let second = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.target"}),
    );
    let (first, second) = tokio::join!(first, second);

    let ok_count = [first["ok"].as_bool(), second["ok"].as_bool()]
        .into_iter()
        .filter(|ok| *ok == Some(true))
        .count();
    let ceiling_count = [&first, &second]
        .into_iter()
        .filter(|envelope| envelope["error"]["code"] == "token-ceiling-exceeded")
        .count();
    assert_eq!(
        ok_count, 1,
        "exactly one cold summary should reserve budget"
    );
    assert_eq!(
        ceiling_count, 1,
        "the overlapping cold summary should fail before provider dispatch"
    );
    assert_eq!(
        provider.invocations().len(),
        1,
        "budget reservation should block the second provider call"
    );

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn summary_cache_hits_survive_blocked_budget() {
    let (project, db_path) = open_project();
    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnySummaryProvider::new_slow(0, 120, 120));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        LlmConfig {
            session_token_ceiling: 150,
            ..llm_config()
        },
    );

    let first = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(first["ok"], true);
    assert_eq!(first["result"]["cache"]["hit"], false);

    let blocked = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.target"}),
    )
    .await;
    assert_eq!(blocked["ok"], false);
    assert_eq!(blocked["error"]["code"], "token-ceiling-exceeded");

    let cached = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(cached["ok"], true);
    assert_eq!(cached["result"]["cache"]["hit"], true);
    assert_eq!(
        provider.invocations().len(),
        1,
        "blocked budget should not prevent cached summaries from being served"
    );

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn entity_at_returns_innermost_entity_and_empty_match() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let hit = call_tool(&state, "entity_at", json!({"file": "demo.py", "line": 1})).await;
    assert_eq!(hit["ok"], true);
    assert_eq!(hit["result"]["entity"]["id"], "python:function:demo.entry");

    let miss = call_tool(&state, "entity_at", json!({"file": "demo.py", "line": 99})).await;
    assert_eq!(miss["ok"], true);
    assert!(miss["result"]["entity"].is_null());
}

#[tokio::test]
async fn find_entity_paginates_and_searches_punctuation_heavy_ids() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let first = call_tool(
        &state,
        "find_entity",
        json!({"pattern": "python:function:demo", "limit": 2}),
    )
    .await;
    assert_eq!(first["ok"], true);
    assert_eq!(first["result"]["entities"].as_array().unwrap().len(), 2);
    assert_eq!(first["result"]["next_cursor"], "2");

    let second = call_tool(
        &state,
        "find_entity",
        json!({"pattern": "python:function:demo", "limit": 2, "cursor": "2"}),
    )
    .await;
    assert_eq!(second["ok"], true);
    assert!(!second["result"]["entities"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn callers_of_defaults_to_resolved_and_expands_ambiguous_candidates() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let default_callers = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.alt_target"}),
    )
    .await;
    assert_eq!(default_callers["ok"], true);
    assert_eq!(
        default_callers["result"]["callers"]
            .as_array()
            .unwrap()
            .len(),
        0
    );

    let ambiguous_callers = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.alt_target", "confidence": "ambiguous"}),
    )
    .await;
    assert_eq!(ambiguous_callers["ok"], true);
    assert_eq!(
        ambiguous_callers["result"]["callers"][0]["entity"]["id"],
        "python:function:demo.entry"
    );
    assert_eq!(
        ambiguous_callers["result"]["callers"][0]["stored_to_id"],
        "python:function:demo.target"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callers_of_inferred_dispatches_and_materializes_recording_result() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    let source_path = project.path().join("demo.py");
    insert_entity(
        &conn,
        "python:function:demo.dynamic",
        "function",
        &source_path,
        Some((9, 10)),
        Some("python:module:demo"),
    );
    insert_unresolved_call_site(
        &conn,
        "python:function:demo.entry",
        "site-dynamic",
        "dynamic",
    );
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnyInferredProvider::new(
        r#"{"edges":[{"site_key":"site-dynamic","target_id":"python:function:demo.dynamic","confidence":0.91,"rationale":"name match"}]}"#,
    ));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let envelope = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.dynamic", "confidence": "inferred"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(
        envelope["result"]["callers"][0]["entity"]["id"],
        "python:function:demo.entry"
    );
    assert_eq!(
        envelope["result"]["callers"][0]["edge_confidence"],
        "inferred"
    );
    assert_eq!(envelope["stats_delta"]["inferred_dispatch_misses_total"], 1);
    assert_eq!(provider.invocations().len(), 1);
    assert_eq!(provider.invocations()[0].purpose, LlmPurpose::InferredEdges);
    assert_eq!(
        provider.invocations()[0].prompt_id,
        INFERRED_CALLS_PROMPT_VERSION
    );

    let warm = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.dynamic", "confidence": "inferred"}),
    )
    .await;
    assert_eq!(warm["ok"], true);
    assert_eq!(provider.invocations().len(), 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inferred_dispatch_prompt_uses_caller_source_range_not_whole_file() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    let source_path = project.path().join("demo.py");
    insert_entity(
        &conn,
        "python:function:demo.dynamic",
        "function",
        &source_path,
        Some((1, 2)),
        Some("python:module:demo"),
    );
    insert_unresolved_call_site(
        &conn,
        "python:function:demo.dynamic",
        "site-dynamic",
        "target",
    );
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnyInferredProvider::new(
        r#"{"edges":[{"site_key":"site-dynamic","target_id":"python:function:demo.target","confidence":0.91,"rationale":"name match"}]}"#,
    ));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let envelope = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.target", "confidence": "inferred"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    let invocation = provider
        .invocations()
        .into_iter()
        .next()
        .expect("inferred provider invocation");
    assert!(invocation.prompt.contains("def entry():"));
    assert!(
        !invocation.prompt.contains("def mid():"),
        "inferred prompt leaked neighboring function source: {}",
        invocation.prompt
    );

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callers_of_inferred_coalesces_concurrent_cold_requests() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    let source_path = project.path().join("demo.py");
    insert_entity(
        &conn,
        "python:function:demo.dynamic",
        "function",
        &source_path,
        Some((9, 10)),
        Some("python:module:demo"),
    );
    insert_unresolved_call_site(
        &conn,
        "python:function:demo.entry",
        "site-dynamic",
        "dynamic",
    );
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnyInferredProvider::new_slow(
        r#"{"edges":[{"site_key":"site-dynamic","target_id":"python:function:demo.dynamic","confidence":0.91,"rationale":"name match"}]}"#,
        100,
    ));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );
    let args = json!({"id": "python:function:demo.dynamic", "confidence": "inferred"});

    let (first, second) = tokio::join!(
        call_tool(&state, "callers_of", args.clone()),
        call_tool(&state, "callers_of", args),
    );

    assert_eq!(first["ok"], true);
    assert_eq!(second["ok"], true);
    assert_eq!(provider.invocations().len(), 1);
    let coalesced_total = first["stats_delta"]["inferred_dispatch_coalesced_total"]
        .as_u64()
        .unwrap_or(0)
        + second["stats_delta"]["inferred_dispatch_coalesced_total"]
            .as_u64()
            .unwrap_or(0);
    assert_eq!(coalesced_total, 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callers_of_inferred_invalid_json_preserves_usage_accounting() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    let source_path = project.path().join("demo.py");
    insert_entity(
        &conn,
        "python:function:demo.dynamic",
        "function",
        &source_path,
        Some((9, 10)),
        Some("python:module:demo"),
    );
    insert_unresolved_call_site(
        &conn,
        "python:function:demo.entry",
        "site-dynamic",
        "dynamic",
    );
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnyInferredProvider::new("not-json"));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let envelope = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.dynamic", "confidence": "inferred"}),
    )
    .await;

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "llm-invalid-json");
    assert_eq!(envelope["stats_delta"]["inferred_dispatch_misses_total"], 1);
    assert_eq!(envelope["stats_delta"]["inferred_tokens_input"], 100);
    assert_eq!(envelope["stats_delta"]["inferred_tokens_output"], 20);
    assert_eq!(envelope["stats_delta"]["inferred_tokens_total"], 120);
    assert_eq!(envelope["stats_delta"]["inferred_cost_usd"], 0.0);
    assert_eq!(envelope["stats_delta"]["llm_invalid_json_total"], 1);
    assert_eq!(provider.invocations().len(), 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn execution_paths_from_reports_edge_cap_truncation() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path).with_edge_cap(1);

    let envelope = call_tool(
        &state,
        "execution_paths_from",
        json!({"id": "python:function:demo.entry", "max_depth": 3, "confidence": "ambiguous"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["truncated"], true);
    assert_eq!(envelope["truncation_reason"], "edge-cap");
    assert_eq!(envelope["result"]["edge_count_visited"], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_paths_from_inferred_dispatches_start_caller() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    let source_path = project.path().join("demo.py");
    insert_entity(
        &conn,
        "python:function:demo.dynamic",
        "function",
        &source_path,
        Some((9, 10)),
        Some("python:module:demo"),
    );
    insert_unresolved_call_site(
        &conn,
        "python:function:demo.entry",
        "site-dynamic",
        "dynamic",
    );
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnyInferredProvider::new(
        r#"{"edges":[{"site_key":"site-dynamic","target_id":"python:function:demo.dynamic","confidence":0.91,"rationale":"name match"}]}"#,
    ));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let envelope = call_tool(
        &state,
        "execution_paths_from",
        json!({"id": "python:function:demo.entry", "max_depth": 1, "confidence": "inferred"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert!(
        envelope["result"]["paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| {
                path.as_array()
                    .unwrap()
                    .iter()
                    .any(|node| node["id"] == "python:function:demo.dynamic")
            })
    );
    assert_eq!(provider.invocations().len(), 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_paths_from_inferred_dispatches_reached_callers() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    let source_path = project.path().join("demo.py");
    insert_entity(
        &conn,
        "python:function:demo.dynamic",
        "function",
        &source_path,
        Some((9, 10)),
        Some("python:module:demo"),
    );
    insert_unresolved_call_site(&conn, "python:function:demo.mid", "site-dynamic", "dynamic");
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = inferred_recording(
        project.path(),
        "python:function:demo.mid",
        "site-dynamic",
        "dynamic",
        "python:function:demo.dynamic",
    );
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let envelope = call_tool(
        &state,
        "execution_paths_from",
        json!({"id": "python:function:demo.entry", "max_depth": 2, "confidence": "inferred"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert!(
        envelope["result"]["paths"]
            .as_array()
            .unwrap()
            .iter()
            .any(|path| {
                path.as_array()
                    .unwrap()
                    .iter()
                    .any(|node| node["id"] == "python:function:demo.dynamic")
            })
    );
    assert_eq!(provider.invocations().len(), 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn neighborhood_returns_one_hop_graph_sections() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:demo.target", "confidence": "resolved"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(
        envelope["result"]["callers"][0]["entity"]["id"],
        "python:function:demo.mid"
    );
    assert_eq!(envelope["result"]["container"]["id"], "python:module:demo");
    assert_eq!(envelope["result"]["contained"].as_array().unwrap().len(), 0);
    assert_eq!(
        envelope["result"]["references_in"][0]["entity"]["id"],
        "python:function:demo.entry"
    );
}
