//! MCP storage-backed tool tests.

use std::{
    fs,
    sync::{Arc, Mutex},
};

use clarion_core::{
    CachingModel, INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput,
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID, LeafSummaryPromptInput, LlmProvider, LlmProviderError,
    LlmPurpose, LlmRequest, LlmResponse, OpenRouterProvider, OpenRouterProviderConfig, Recording,
    RecordingProvider, build_inferred_calls_prompt, build_leaf_summary_prompt,
};
use clarion_mcp::{
    DiagnosticsContext, LlmDiagnostics, ServerState,
    config::{FiligreeConfig, LlmConfig, LlmProviderKind},
    filigree::{
        EntityAssociation, EntityAssociationsResponse, FiligreeClientError, FiligreeLookup,
        IssueDetail, WardlineFinding,
    },
    filigree_url::{SOURCE_CONFIG, SOURCE_EPHEMERAL_PORT, resolve_filigree_url},
    list_tools,
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

fn add_dynamic_source(project_root: &std::path::Path) {
    std::fs::write(
        project_root.join("demo.py"),
        "def entry():\n    return mid()\n\ndef mid():\n    return target()\n\ndef target():\n    return 1\n\ndef dynamic():\n    return target()\n",
    )
    .expect("write dynamic source");
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
    let content_hash = fixture_content_hash(kind, source_path, range);
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
            content_hash,
        ],
    )
    .expect("insert entity");
}

/// Insert an entity carrying a non-empty `properties` JSON blob (e.g. the
/// `definition` sub-range evidence the Python plugin records). Mirrors
/// [`insert_entity`] but lets a test seed `properties_json` directly.
fn insert_entity_with_properties(
    conn: &Connection,
    id: &str,
    kind: &str,
    source_path: &std::path::Path,
    range: Option<(i64, i64)>,
    parent_id: Option<&str>,
    properties_json: &str,
) {
    let content_hash = fixture_content_hash(kind, source_path, range);
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash, created_at, updated_at
         ) VALUES (
            ?1, 'python', ?2, ?1, ?1, ?3, ?4, ?5, ?6, ?8, ?7,
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
            content_hash,
            properties_json,
        ],
    )
    .expect("insert entity with properties");
}

fn fixture_content_hash(
    kind: &str,
    source_path: &std::path::Path,
    range: Option<(i64, i64)>,
) -> String {
    if kind == "module" {
        return blake3::hash(&std::fs::read(source_path).expect("read module source"))
            .to_hex()
            .to_string();
    }
    let source = std::fs::read_to_string(source_path).expect("read entity source");
    let (start_line, end_line) = range.expect("non-module fixture has source range");
    let start = usize::try_from(start_line - 1).expect("start line fits usize");
    let mut end = usize::try_from(end_line).expect("end line fits usize");
    let lines = source.lines().collect::<Vec<_>>();
    end = end.min(lines.len());
    assert!(start < end, "fixture range must overlap source");
    let normalized = lines[start..end].join("\n");
    blake3::hash(normalized.as_bytes()).to_hex().to_string()
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
    let caller_content_hash: String = conn
        .query_row(
            "SELECT content_hash FROM entities WHERE id = ?1",
            params![caller_id],
            |row| row.get(0),
        )
        .expect("caller content hash");
    conn.execute(
        "INSERT INTO entity_unresolved_call_sites (
            caller_entity_id, caller_content_hash, site_key, site_ordinal,
            source_file_id, source_byte_start, source_byte_end, callee_expr, created_at
         ) VALUES (?1, ?2, ?3, 0, 'python:module:demo', 30, 37, ?4, '2026-05-17T00:00:00.000Z')",
        params![caller_id, caller_content_hash, site_key, expr],
    )
    .expect("insert unresolved call site");
}

fn seed_subsystem(conn: &Connection, project_root: &std::path::Path) -> String {
    let extra_source_path = project_root.join("pkg_auth.py");
    std::fs::write(&extra_source_path, "def login():\n    return True\n")
        .expect("write extra module source");
    insert_entity(
        conn,
        "python:module:pkg.auth",
        "module",
        &extra_source_path,
        Some((1, 2)),
        None,
    );

    let subsystem_id = "core:subsystem:abc123def456".to_owned();
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, properties, created_at, updated_at
         ) VALUES (
            ?1, 'core', 'subsystem', 'Subsystem abc123def456', 'abc123def456',
            ?2, '2026-05-17T00:00:00.000Z', '2026-05-17T00:00:00.000Z'
         )",
        params![
            subsystem_id,
            json!({"member_count": 2, "modularity_score": 0.42}).to_string(),
        ],
    )
    .expect("insert subsystem entity");
    insert_edge(
        conn,
        "in_subsystem",
        "python:module:demo",
        &subsystem_id,
        "resolved",
        None,
    );
    insert_edge(
        conn,
        "in_subsystem",
        "python:module:pkg.auth",
        &subsystem_id,
        "resolved",
        None,
    );
    subsystem_id
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
            cached_input_tokens: 0,
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
    let caller_content_hash = expected_content_hash(project_root, caller_id);
    let target_content_hash = expected_content_hash(project_root, target_id);
    let unresolved_call_sites_json = serde_json::to_string(&vec![json!({
        "caller_entity_id": caller_id,
        "caller_content_hash": caller_content_hash,
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
        "content_hash": target_content_hash
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

fn expected_content_hash(project_root: &std::path::Path, entity_id: &str) -> String {
    let kind = if entity_id == "python:module:demo" {
        "module"
    } else {
        "function"
    };
    fixture_content_hash(
        kind,
        &project_root.join("demo.py"),
        expected_line_range(entity_id),
    )
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
            cached_input_tokens: 0,
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
            cached_input_tokens: 0,
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
            cached_input_tokens: 0,
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
    /// `issue_id` -> detail returned by `issue_detail`; absent ids yield `Ok(None)`.
    details: Mutex<std::collections::HashMap<String, IssueDetail>>,
    /// `issue_id`s `issue_detail` was called with, in order — proves dedup/N+1.
    detail_calls: Mutex<Vec<String>>,
    /// Wardline findings returned by `wardline_findings_for_path`.
    wardline_findings: Mutex<Vec<WardlineFinding>>,
    /// When true, `wardline_findings_for_path` returns an `HttpStatus` 503 error.
    wardline_error: Mutex<bool>,
}

impl FakeFiligreeClient {
    fn with_response(mut self, entity_id: &str, associations: Vec<EntityAssociation>) -> Self {
        self.responses.get_mut().unwrap().insert(
            entity_id.to_owned(),
            EntityAssociationsResponse { associations },
        );
        self
    }

    fn with_detail(mut self, issue_id: &str, title: &str, status: &str, priority: i64) -> Self {
        self.details.get_mut().unwrap().insert(
            issue_id.to_owned(),
            IssueDetail {
                title: title.to_owned(),
                status: status.to_owned(),
                priority,
            },
        );
        self
    }

    fn with_wardline_findings(mut self, findings: Vec<WardlineFinding>) -> Self {
        *self.wardline_findings.get_mut().unwrap() = findings;
        self
    }

    fn with_wardline_error(mut self) -> Self {
        *self.wardline_error.get_mut().unwrap() = true;
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn detail_calls(&self) -> Vec<String> {
        self.detail_calls
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

    fn issue_detail(&self, issue_id: &str) -> Result<Option<IssueDetail>, FiligreeClientError> {
        self.detail_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(issue_id.to_owned());
        Ok(self
            .details
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(issue_id)
            .cloned())
    }

    fn wardline_findings_for_path(
        &self,
        _path: &str,
    ) -> Result<Vec<WardlineFinding>, FiligreeClientError> {
        if *self
            .wardline_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(FiligreeClientError::HttpStatus {
                status: 503,
                body: "down".to_owned(),
            });
        }
        Ok(self
            .wardline_findings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
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
        .await
        .expect("tools/call request returns a response");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], "tool-test");
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool content text");
    serde_json::from_str(text).expect("tool envelope JSON")
}

#[test]
fn tools_list_includes_subsystem_members() {
    let tools = list_tools();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "subsystem_members")
        .expect("subsystem_members tool definition");

    assert_eq!(
        tool.description,
        "List module entities assigned to a subsystem entity."
    );
    assert_eq!(
        tool.input_schema,
        json!({
            "type": "object",
            "properties": {
                "id": {"type": "string", "minLength": 1}
            },
            "required": ["id"],
            "additionalProperties": false
        })
    );
}

#[tokio::test]
async fn subsystem_members_returns_member_modules() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    let subsystem_id = seed_subsystem(&conn, project.path());
    drop(conn);
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(&state, "subsystem_members", json!({"id": subsystem_id})).await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(
        envelope["result"]["subsystem"]["id"],
        "core:subsystem:abc123def456"
    );
    assert_eq!(
        envelope["result"]["subsystem"]["name"],
        "Subsystem abc123def456"
    );
    assert_eq!(
        envelope["result"]["subsystem"]["short_name"],
        "abc123def456"
    );
    assert_eq!(
        envelope["result"]["subsystem"]["properties"]["member_count"],
        2
    );
    assert_eq!(
        envelope["result"]["subsystem"]["properties"]["modularity_score"],
        0.42
    );
    let members = envelope["result"]["members"].as_array().unwrap();
    assert_eq!(members.len(), 2);
    assert_eq!(members[0]["id"], "python:module:demo");
    assert_eq!(members[1]["id"], "python:module:pkg.auth");
    assert!(
        members[0]["source_file_path"]
            .as_str()
            .unwrap()
            .ends_with("demo.py")
    );
}

#[tokio::test]
async fn subsystem_members_rejects_non_subsystem_id() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "subsystem_members",
        json!({"id": "python:module:demo"}),
    )
    .await;

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "not-a-subsystem");
    assert_eq!(envelope["result"], Value::Null);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_on_subsystem_returns_policy_envelope_without_llm_call() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    let subsystem_id = seed_subsystem(&conn, project.path());
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnySummaryProvider::new_output(
        r#"{"purpose":"should not run"}"#,
        120,
        0.012,
    ));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let envelope = call_tool(&state, "summary", json!({"id": subsystem_id})).await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["available"], false);
    assert_eq!(envelope["result"]["reason"], "summary-scope-deferred");
    assert_eq!(envelope["stats_delta"], json!({}));
    assert!(provider.invocations().is_empty());

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_on_secret_blocked_entity_returns_policy_envelope_without_llm_or_cache() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "UPDATE entities SET properties = ?1 WHERE id = 'python:function:demo.entry'",
        params![json!({"briefing_blocked": "secret_present"}).to_string()],
    )
    .expect("mark entity blocked");
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnySummaryProvider::new_output(
        r#"{"purpose":"should not run"}"#,
        120,
        0.012,
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
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["summary"], Value::Null);
    assert_eq!(envelope["result"]["briefing_blocked"], "secret_present");
    assert_eq!(envelope["stats_delta"], json!({}));
    assert!(provider.invocations().is_empty());

    let entity_at = call_tool(&state, "entity_at", json!({"file": "demo.py", "line": 1})).await;
    assert_eq!(entity_at["ok"], true);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();

    let conn = Connection::open(&db_path).expect("open sqlite");
    let cache_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM summary_cache WHERE entity_id = 'python:function:demo.entry'",
            [],
            |row| row.get(0),
        )
        .expect("query summary cache");
    assert_eq!(cache_rows, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_on_unscanned_source_returns_policy_envelope_without_llm_or_cache() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "UPDATE entities SET properties = ?1 WHERE id = 'python:function:demo.entry'",
        params![json!({"briefing_blocked": "unscanned_source"}).to_string()],
    )
    .expect("mark entity blocked");
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnySummaryProvider::new_output(
        r#"{"purpose":"should not run"}"#,
        120,
        0.012,
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
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["summary"], Value::Null);
    assert_eq!(envelope["result"]["briefing_blocked"], "unscanned_source");
    assert!(
        envelope["result"]["remediation"]
            .as_str()
            .expect("remediation text")
            .contains("not covered by the pre-ingest secret scan")
    );
    assert_eq!(envelope["stats_delta"], json!({}));
    assert!(provider.invocations().is_empty());

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
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
    // Unavailable is now an explicit result_kind, distinct from no_matches.
    assert_eq!(envelope["result"]["result_kind"], "unavailable");
}

#[tokio::test]
async fn issues_for_reports_resolved_endpoint_and_result_kind() {
    // AC#1/#2: issues_for surfaces the configured vs resolved (ethereal-port)
    // endpoint, and distinguishes reachable-but-empty (no_matches) from a
    // populated result (matched) — without the agent curling ports by hand.
    let (project, db_path) = open_project();
    let filigree_dir = project.path().join(".filigree");
    fs::create_dir_all(&filigree_dir).unwrap();
    fs::write(filigree_dir.join("ephemeral.port"), "8542").unwrap();
    let config = FiligreeConfig {
        enabled: true,
        ..FiligreeConfig::default()
    };
    let diagnostics = DiagnosticsContext {
        llm: LlmDiagnostics {
            provider: "disabled".to_owned(),
            live: false,
            allow_live_provider: false,
            cache_max_age_days: 180,
        },
        filigree: resolve_filigree_url(&config, project.path()),
    };

    // Reachable but no associations for this entity -> no_matches.
    let empty_client = Arc::new(FakeFiligreeClient::default());
    let state = state_for_filigree(project.path(), &db_path, empty_client)
        .with_diagnostics(diagnostics.clone());
    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.entry", "include_contained": false}),
    )
    .await;
    assert_eq!(envelope["result"]["available"], true);
    assert_eq!(envelope["result"]["result_kind"], "no_matches");
    let endpoint = &envelope["result"]["filigree_endpoint"];
    assert_eq!(endpoint["configured_url"], "http://127.0.0.1:8766");
    assert_eq!(endpoint["resolved_url"], "http://127.0.0.1:8542");
    assert_eq!(endpoint["resolution_source"], SOURCE_EPHEMERAL_PORT);

    // A populated result -> matched, same endpoint block.
    let client = Arc::new(FakeFiligreeClient::default().with_response(
        "python:function:demo.entry",
        vec![association(
            "filigree-fresh",
            "python:function:demo.entry",
            &expected_content_hash(project.path(), "python:function:demo.entry"),
        )],
    ));
    let state = state_for_filigree(project.path(), &db_path, client).with_diagnostics(diagnostics);
    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.entry", "include_contained": false}),
    )
    .await;
    assert_eq!(envelope["result"]["result_kind"], "matched");
    assert_eq!(
        envelope["result"]["filigree_endpoint"]["resolved_url"],
        "http://127.0.0.1:8542"
    );
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
                    &expected_content_hash(project.path(), "python:function:demo.entry"),
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
async fn issues_for_enriches_matched_and_drifted_with_issue_detail() {
    // AC: matched/drifted entries carry title/status/priority fetched from
    // Filigree, batched to one request per distinct issue (no N+1), and degrade
    // to a null `issue` when the detail route has no entry for that issue
    // (clarion-51a2868c86).
    let (project, db_path) = open_project();
    let client = Arc::new(
        FakeFiligreeClient::default()
            .with_response(
                "python:function:demo.entry",
                vec![association(
                    "filigree-fresh",
                    "python:function:demo.entry",
                    &expected_content_hash(project.path(), "python:function:demo.entry"),
                )],
            )
            .with_response(
                "python:function:demo.mid",
                vec![association(
                    "filigree-drifted",
                    "python:function:demo.mid",
                    "old-hash",
                )],
            )
            // Detail present for the matched issue, absent for the drifted one.
            .with_detail("filigree-fresh", "Refresh tokens", "building", 1),
    );
    let state = state_for_filigree(project.path(), &db_path, client.clone());

    let envelope = call_tool(&state, "issues_for", json!({"id": "python:module:demo"})).await;

    assert_eq!(envelope["ok"], true);
    // Matched entry carries the fetched detail.
    let matched = &envelope["result"]["matched"][0];
    assert_eq!(matched["issue_id"], "filigree-fresh");
    assert_eq!(matched["issue"]["title"], "Refresh tokens");
    assert_eq!(matched["issue"]["status"], "building");
    assert_eq!(matched["issue"]["priority"], 1);
    // Drifted entry has no configured detail → null, but still resolves.
    let drifted = &envelope["result"]["drifted"][0];
    assert_eq!(drifted["issue_id"], "filigree-drifted");
    assert_eq!(drifted["issue"], Value::Null);
    // Each distinct issue is fetched exactly once (no N+1).
    let mut detail_calls = client.detail_calls();
    detail_calls.sort();
    assert_eq!(
        detail_calls,
        vec!["filigree-drifted".to_owned(), "filigree-fresh".to_owned()]
    );
    assert_eq!(
        envelope["stats_delta"]["filigree_detail_requests_total"], 2,
        "two distinct issues -> two detail requests: {envelope}"
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
                    &expected_content_hash(project.path(), "python:module:demo"),
                )],
            )
            .with_response(
                "python:function:demo.entry",
                vec![association(
                    "filigree-entry",
                    "python:function:demo.entry",
                    &expected_content_hash(project.path(), "python:function:demo.entry"),
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
                &expected_content_hash(project.path(), "python:function:demo.entry"),
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
                &expected_content_hash(project.path(), "python:module:demo"),
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
                    &expected_content_hash(project.path(), "python:function:demo.entry"),
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
async fn summary_invalid_json_falls_back_to_structural_summary() {
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

    // Invalid provider JSON degrades to a deterministic structural summary
    // instead of an error (clarion-ed246ca3aa).
    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(envelope["result"]["available"], true);
    assert_eq!(envelope["result"]["summary"]["kind"], "structural-fallback");
    assert!(envelope["result"]["summary"]["source_head"].is_string());
    // The single real call is still accounted (honest), flagged as a fallback.
    assert_eq!(envelope["stats_delta"]["summary_cost_usd"], 0.012);
    assert_eq!(envelope["stats_delta"]["llm_invalid_json_total"], 1);
    assert_eq!(
        envelope["stats_delta"]["summary_structural_fallback_total"],
        1
    );
    assert_eq!(provider.invocations().len(), 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_invalid_json_fallback_is_cached_and_not_rebilled() {
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

    let first = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(provider.invocations().len(), 1);

    // A repeat request must hit the cached fallback: no second LLM call, no
    // second bill — the deterministic-failure billing loop is closed.
    let second = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(second["result"]["cache"]["hit"], true);
    assert_eq!(second["result"]["summary"]["kind"], "structural-fallback");
    assert_eq!(second["stats_delta"]["summary_cache_hits_total"], 1);
    assert!(
        second["stats_delta"].get("summary_cost_usd").is_none(),
        "cache hit must not bill again: {second}"
    );
    assert_eq!(
        provider.invocations().len(),
        1,
        "no second provider invocation"
    );

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
            referer: "https://github.com/tachyon-beep/clarion".to_owned(),
            title: "Clarion Test".to_owned(),
            timeout_seconds: 30,
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
async fn summary_cold_miss_refuses_live_source_drift() {
    let (project, db_path) = open_project();
    let original_hash = blake3::hash("def entry():\n    return mid()".as_bytes())
        .to_hex()
        .to_string();
    let conn = Connection::open(&db_path).unwrap();
    conn.execute(
        "UPDATE entities SET content_hash = ?1 WHERE id = 'python:function:demo.entry'",
        params![original_hash],
    )
    .unwrap();
    drop(conn);
    fs::write(
        project.path().join("demo.py"),
        "def entry():\n    return changed()\n\ndef mid():\n    return target()\n\ndef target():\n    return 1\n",
    )
    .unwrap();

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnySummaryProvider::new_output(
        r#"{"purpose":"should not run"}"#,
        120,
        0.0,
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
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "content-drift");
    assert_eq!(provider.invocations().len(), 0);

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
                content_hash: expected_content_hash(project.path(), "python:function:demo.entry"),
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

#[tokio::test]
async fn summary_preview_cost_reports_cache_hit_without_llm_call() {
    // AC: a cached summary reports cache_hit (with the row's real tokens/cost)
    // and never dispatches to the provider.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    upsert_summary_cache(
        &conn,
        &SummaryCacheEntry {
            key: SummaryCacheKey {
                entity_id: "python:function:demo.entry".to_owned(),
                content_hash: expected_content_hash(project.path(), "python:function:demo.entry"),
                prompt_template_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                model_tier: "anthropic/claude-sonnet-4.6".to_owned(),
                guidance_fingerprint: "guidance-empty".to_owned(),
            },
            summary_json: r#"{"purpose":"cached"}"#.to_owned(),
            cost_usd: 0.0021,
            tokens_input: 123,
            tokens_output: 45,
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
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let envelope = call_tool(
        &state,
        "summary_preview_cost",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["cache_status"], "hit");
    assert_eq!(envelope["result"]["cached"]["tokens_input"], 123);
    assert_eq!(envelope["result"]["cached"]["tokens_output"], 45);
    assert_eq!(envelope["result"]["cached"]["cost_usd"], 0.0021);
    assert_eq!(envelope["result"]["cached"]["age_days"], 0);
    assert_eq!(envelope["result"]["live_spend_would_occur"], false);
    // No estimate needed on a hit, and crucially: no provider call.
    assert!(envelope["result"]["estimated_input_tokens"].is_null());
    assert!(
        provider.invocations().is_empty(),
        "preview must never call the LLM provider"
    );

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn summary_preview_cost_reports_miss_estimate_and_live_spend() {
    // AC: a cache miss reports provider/model + a token estimate, flags that a
    // live call would spend, and still never calls the provider.
    let (project, db_path) = open_project();
    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(RecordingProvider::from_recordings(Vec::new()));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let envelope = call_tool(
        &state,
        "summary_preview_cost",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["cache_status"], "miss");
    assert!(envelope["result"]["cached"].is_null());
    assert_eq!(
        envelope["result"]["model_id"],
        "anthropic/claude-sonnet-4.6"
    );
    assert!(
        envelope["result"]["estimated_input_tokens"]
            .as_i64()
            .is_some_and(|tokens| tokens > 0),
        "miss should carry a positive input-token estimate: {envelope:?}"
    );
    assert_eq!(envelope["result"]["estimated_output_tokens"], 512);
    assert_eq!(envelope["result"]["policy"]["live"], true);
    assert_eq!(envelope["result"]["live_spend_would_occur"], true);
    assert!(
        provider.invocations().is_empty(),
        "preview must never call the LLM provider"
    );

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn summary_preview_cost_disabled_llm_is_distinct_from_miss() {
    // AC: a disabled/unconfigured LLM is reported distinctly from a cache miss —
    // a miss with no live provider would NOT spend.
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path); // no LLM wired

    let envelope = call_tool(
        &state,
        "summary_preview_cost",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["cache_status"], "miss");
    assert_eq!(envelope["result"]["policy"]["live"], false);
    assert_eq!(envelope["result"]["policy"]["enabled"], false);
    assert_eq!(
        envelope["result"]["live_spend_would_occur"], false,
        "a miss with no live provider must not be flagged as spending: {envelope:?}"
    );
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
                content_hash: expected_content_hash(project.path(), "python:function:demo.entry"),
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

    // entity_context evidence: the containing stack runs module → matched
    // entity, and without recorded sub-ranges the reason is the honest
    // containing_range (the seed fixture carries no `definition` block).
    let ctx = &hit["result"]["entity_context"];
    assert_eq!(ctx["query_line"], 1);
    assert_eq!(ctx["match_reason"], "containing_range");
    let stack = ctx["containing_stack"].as_array().expect("stack array");
    assert_eq!(stack.first().unwrap()["id"], "python:module:demo");
    assert_eq!(stack.last().unwrap()["id"], "python:function:demo.entry");
    assert!(ctx["freshness"].is_object());

    let miss = call_tool(&state, "entity_at", json!({"file": "demo.py", "line": 99})).await;
    assert_eq!(miss["ok"], true);
    assert!(miss["result"]["entity"].is_null());
    assert_eq!(miss["result"]["entity_context"]["match_reason"], "no_match");
    assert!(
        miss["result"]["entity_context"]["containing_stack"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn entity_at_blank_line_reports_containing_range_not_a_fabricated_match() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    // Line 3 of demo.py is the blank line between demo.entry (1-2) and
    // demo.mid (4-5). Only the module spans it, so the honest answer is the
    // module with match_reason=containing_range — never a nearby function
    // dressed up as an exact match (clarion-460def6a51 acceptance #3).
    let resp = call_tool(&state, "entity_at", json!({"file": "demo.py", "line": 3})).await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    assert_eq!(resp["result"]["entity"]["id"], "python:module:demo");
    assert_eq!(resp["result"]["entity"]["kind"], "module");
    assert_eq!(
        resp["result"]["entity_context"]["match_reason"],
        "containing_range"
    );
}

#[tokio::test]
async fn entity_at_reports_same_span_ambiguity_alternatives() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    // demo.target and demo.alt_target both span lines 7-8 in the seed graph.
    // The innermost winner is chosen by the id tie-break; the other surfaces
    // as a same-granularity ambiguity alternative.
    let resp = call_tool(&state, "entity_at", json!({"file": "demo.py", "line": 7})).await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    assert_eq!(
        resp["result"]["entity"]["id"],
        "python:function:demo.alt_target"
    );
    let alternatives = resp["result"]["entity_context"]["alternatives"]
        .as_array()
        .expect("alternatives array");
    let alt_ids: Vec<&str> = alternatives
        .iter()
        .map(|a| a["entity"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(alt_ids, vec!["python:function:demo.target"]);
}

#[tokio::test]
async fn entity_at_explains_decorator_declaration_and_body_matches() {
    let (project, db_path) = open_project();

    // A decorated class in runtime.py: `@dataclass` on line 1, `class Config:`
    // on line 2, the field body on line 3. The plugin expands the span to the
    // decorator line and records the sub-ranges in `properties.definition`.
    let source = "@dataclass\nclass Config:\n    retries: int = 3\n";
    let runtime_path = project.path().join("runtime.py");
    std::fs::write(&runtime_path, source).expect("write runtime.py");
    {
        let conn = Connection::open(&db_path).expect("open db");
        insert_entity_with_properties(
            &conn,
            "python:module:runtime",
            "module",
            &runtime_path,
            Some((1, 3)),
            None,
            "{}",
        );
        insert_entity_with_properties(
            &conn,
            "python:class:runtime.Config",
            "class",
            &runtime_path,
            Some((1, 3)),
            Some("python:module:runtime"),
            &json!({
                "definition": {
                    "decl_line": 2,
                    "body_line_start": 3,
                    "decorator_line_start": 1,
                    "decorator_line_end": 1
                }
            })
            .to_string(),
        );
    }
    let state = state_for(project.path(), &db_path);

    // Decorator line resolves to the class, explained as decorator_range.
    let decorator_hit = call_tool(
        &state,
        "entity_at",
        json!({"file": "runtime.py", "line": 1}),
    )
    .await;
    assert_eq!(decorator_hit["ok"], true, "{decorator_hit:?}");
    assert_eq!(
        decorator_hit["result"]["entity"]["id"],
        "python:class:runtime.Config"
    );
    assert_eq!(
        decorator_hit["result"]["entity_context"]["match_reason"],
        "decorator_range"
    );
    assert_eq!(
        decorator_hit["result"]["entity_context"]["ranges"]["decl_line"],
        2
    );

    // The declaration line resolves to the same class with a distinct reason.
    let declaration_hit = call_tool(
        &state,
        "entity_at",
        json!({"file": "runtime.py", "line": 2}),
    )
    .await;
    assert_eq!(
        declaration_hit["result"]["entity"]["id"],
        "python:class:runtime.Config"
    );
    assert_eq!(
        declaration_hit["result"]["entity_context"]["match_reason"],
        "declaration"
    );

    // The body line is classified as body_range.
    let body_hit = call_tool(
        &state,
        "entity_at",
        json!({"file": "runtime.py", "line": 3}),
    )
    .await;
    assert_eq!(
        body_hit["result"]["entity"]["id"],
        "python:class:runtime.Config"
    );
    assert_eq!(
        body_hit["result"]["entity_context"]["match_reason"],
        "body_range"
    );
}

#[tokio::test]
async fn orientation_pack_for_entity_bundles_all_sections_deterministically() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    let result = &resp["result"];

    // Primary entity + source location.
    assert_eq!(result["primary_entity"]["id"], "python:function:demo.entry");
    assert_eq!(result["source"]["source_line_start"], 1);
    assert!(
        result["source"]["source_file_path"]
            .as_str()
            .unwrap()
            .ends_with("demo.py")
    );

    // Neighbors: resolved callee demo.mid and the module container.
    let callee_ids: Vec<&str> = result["neighbors"]["callees"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entity"]["id"].as_str().unwrap())
        .collect();
    assert!(callee_ids.contains(&"python:function:demo.mid"), "{resp:?}");
    assert_eq!(result["neighbors"]["container"]["id"], "python:module:demo");

    // Compact execution paths reach downstream of entry.
    assert!(
        !result["execution_paths"]["paths"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // issues_for state, index health, and bounded-output bookkeeping are all
    // present in the one response.
    assert!(result["issues"].get("available").is_some());
    assert!(result["health"]["index"].is_object());
    assert!(result["omitted"].is_object());
    let suggested = result["suggested_next_reads"].as_array().unwrap();
    assert_eq!(suggested[0]["tool"], "source_for_entity");

    // Filigree is disabled in this fixture → a clear degradation warning, not a
    // silent empty section.
    let warnings: Vec<&str> = result["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect();
    assert!(warnings.iter().any(|w| w.contains("Filigree")), "{resp:?}");

    // Deterministic: the same request yields a byte-identical packet.
    let again = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(resp, again);
}

#[tokio::test]
async fn orientation_pack_explains_decorator_line_match() {
    let (project, db_path) = open_project();

    // Decorated class: `@dataclass` on line 1, `class Config:` on line 2.
    let source = "@dataclass\nclass Config:\n    retries: int = 3\n";
    let runtime_path = project.path().join("runtime.py");
    std::fs::write(&runtime_path, source).expect("write runtime.py");
    {
        let conn = Connection::open(&db_path).expect("open db");
        insert_entity_with_properties(
            &conn,
            "python:module:runtime",
            "module",
            &runtime_path,
            Some((1, 3)),
            None,
            "{}",
        );
        insert_entity_with_properties(
            &conn,
            "python:class:runtime.Config",
            "class",
            &runtime_path,
            Some((1, 3)),
            Some("python:module:runtime"),
            &json!({
                "definition": {
                    "decl_line": 2,
                    "body_line_start": 3,
                    "decorator_line_start": 1,
                    "decorator_line_end": 1
                }
            })
            .to_string(),
        );
    }
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "orientation_pack",
        json!({"file": "runtime.py", "line": 1}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    assert_eq!(
        resp["result"]["primary_entity"]["id"],
        "python:class:runtime.Config"
    );
    assert_eq!(
        resp["result"]["entity_context"]["match_reason"],
        "decorator_range"
    );
}

#[tokio::test]
async fn orientation_pack_degrades_when_no_entity_spans_the_line() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    // demo.py has no entity spanning line 99.
    let resp = call_tool(
        &state,
        "orientation_pack",
        json!({"file": "demo.py", "line": 99}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    assert!(resp["result"]["primary_entity"].is_null());
    assert_eq!(resp["result"]["entity_context"]["match_reason"], "no_match");
    assert!(!resp["result"]["warnings"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn orientation_pack_rejects_ambiguous_input_form() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    // Supplying both `entity` and `file`+`line` is invalid; the dispatcher
    // returns a JSON-RPC error rather than a tool envelope.
    let response = state
        .handle_json_rpc(&json!({
            "jsonrpc": "2.0",
            "id": "ambiguous",
            "method": "tools/call",
            "params": {
                "name": "orientation_pack",
                "arguments": {"entity": "python:function:demo.entry", "file": "demo.py", "line": 1}
            }
        }))
        .await
        .expect("response");
    assert!(response.get("error").is_some(), "{response:?}");
}

#[tokio::test]
async fn orientation_pack_for_module_rolls_up_references() {
    // The packet-assembly path must also roll up module references and surface
    // `via`, not just `neighborhood` (clarion-79d0ff6e14 review).
    let (project, db_path) = open_project();
    {
        let conn = Connection::open(&db_path).expect("reopen db");
        let source_path = project.path().join("consumer.py");
        std::fs::write(
            &source_path,
            "import demo\n\ndef use():\n    return demo.target()\n",
        )
        .expect("write consumer source");
        insert_entity(
            &conn,
            "python:module:consumer",
            "module",
            &source_path,
            Some((1, 4)),
            None,
        );
        insert_entity(
            &conn,
            "python:function:consumer.use",
            "function",
            &source_path,
            Some((3, 4)),
            Some("python:module:consumer"),
        );
        insert_edge(
            &conn,
            "contains",
            "python:module:consumer",
            "python:function:consumer.use",
            "resolved",
            None,
        );
        insert_edge(
            &conn,
            "references",
            "python:function:consumer.use",
            "python:function:demo.target",
            "resolved",
            None,
        );
    }
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:module:demo"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    let neighbors = &resp["result"]["neighbors"];
    assert_eq!(neighbors["references_rolled_up"], true);
    let refs_in = neighbors["references_in"]
        .as_array()
        .expect("references_in");
    assert_eq!(
        refs_in.len(),
        1,
        "external referencer rolls up: {refs_in:?}"
    );
    assert_eq!(refs_in[0]["entity"]["id"], "python:function:consumer.use");
    assert_eq!(refs_in[0]["via"]["id"], "python:function:demo.target");
}

#[tokio::test]
async fn source_for_entity_returns_span_with_line_numbers_and_context() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    // demo.mid is indexed at lines 4-5 of demo.py.
    let resp = call_tool(
        &state,
        "source_for_entity",
        json!({"id": "python:function:demo.mid", "context_lines": 1}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    let result = &resp["result"];
    assert_eq!(result["source_status"], "ok");
    assert_eq!(result["line_start"], 4);
    assert_eq!(result["line_end"], 5);
    assert_eq!(result["truncated"], false);

    // context_lines=1 widens the window to lines 3..6.
    let lines = result["lines"].as_array().expect("lines array");
    assert_eq!(lines.first().unwrap()["number"], 3);
    assert_eq!(lines.last().unwrap()["number"], 6);

    // The entity's own lines carry the exact source text and in_entity=true;
    // the context lines are flagged in_entity=false.
    let by_number = |n: i64| lines.iter().find(|l| l["number"] == n).unwrap();
    assert_eq!(by_number(4)["text"], "def mid():");
    assert_eq!(by_number(4)["in_entity"], true);
    assert_eq!(by_number(5)["text"], "    return target()");
    assert_eq!(by_number(5)["in_entity"], true);
    assert_eq!(by_number(3)["in_entity"], false);
    assert_eq!(by_number(6)["in_entity"], false);
}

#[tokio::test]
async fn source_for_entity_reports_drift_instead_of_stale_snippet() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    // Mutate the source after indexing so the file no longer matches the
    // stored content_hash.
    std::fs::write(
        project.path().join("demo.py"),
        "def entry():\n    return mid()\n\ndef mid():\n    return target()  # changed\n\ndef target():\n    return 1\n",
    )
    .expect("rewrite source");

    let resp = call_tool(
        &state,
        "source_for_entity",
        json!({"id": "python:function:demo.mid"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    assert_eq!(resp["result"]["source_status"], "drifted");
    assert!(resp["result"]["drift"]["stored_content_hash"].is_string());
    assert!(resp["result"]["drift"]["current_content_hash"].is_string());
    // No source lines are handed back when the snippet would be stale.
    assert!(resp["result"].get("lines").is_none());
}

#[tokio::test]
async fn source_for_entity_reports_missing_file_and_unknown_entity() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    std::fs::remove_file(project.path().join("demo.py")).expect("remove source");
    let missing = call_tool(
        &state,
        "source_for_entity",
        json!({"id": "python:function:demo.mid"}),
    )
    .await;
    assert_eq!(missing["ok"], true, "{missing:?}");
    assert_eq!(missing["result"]["source_status"], "missing");

    let unknown = call_tool(
        &state,
        "source_for_entity",
        json!({"id": "python:function:does.not.exist"}),
    )
    .await;
    assert_eq!(unknown["ok"], false, "{unknown:?}");
    assert_eq!(unknown["error"]["code"], "not-found");
}

#[tokio::test]
async fn call_sites_caller_shows_calls_and_references_with_line_text() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    // demo.entry calls mid (resolved) and target (ambiguous, candidate
    // alt_target), and references target (resolved).
    let resp = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:demo.entry", "confidence": "ambiguous"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    assert_eq!(resp["result"]["role"], "caller");

    let sites = resp["result"]["sites"].as_array().expect("sites array");
    assert!(!sites.is_empty(), "{resp:?}");
    // Every site carries the evidence fields, with a resolved byte->line map.
    for site in sites {
        assert!(site["edge_kind"].is_string(), "{site:?}");
        assert!(site["confidence"].is_string(), "{site:?}");
        assert!(
            site["file"].as_str().unwrap().ends_with("demo.py"),
            "{site:?}"
        );
        assert!(site["line"].as_i64().unwrap() >= 1, "{site:?}");
        assert!(site["line_text"].is_string(), "{site:?}");
    }

    let kinds: std::collections::BTreeSet<&str> = sites
        .iter()
        .map(|s| s["edge_kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains("calls") && kinds.contains("references"),
        "{kinds:?}"
    );

    let confidences: std::collections::BTreeSet<&str> = sites
        .iter()
        .map(|s| s["confidence"].as_str().unwrap())
        .collect();
    assert!(
        confidences.contains("resolved") && confidences.contains("ambiguous"),
        "expected both resolved and ambiguous evidence: {confidences:?}"
    );

    let targets: std::collections::BTreeSet<&str> = sites
        .iter()
        .map(|s| s["other_id"].as_str().unwrap())
        .collect();
    assert!(targets.contains("python:function:demo.mid"), "{targets:?}");
}

#[tokio::test]
async fn call_sites_redacts_line_text_for_briefing_blocked_owner() {
    // A call/reference site owned by an entity whose file the pre-ingest
    // scanner marked briefing_blocked must not have its source bytes read into
    // line_text — that would bypass the secret-redaction policy other read
    // paths enforce. demo.entry owns its outgoing sites; mark it blocked.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "UPDATE entities SET properties = ?1 WHERE id = 'python:function:demo.entry'",
        params![json!({"briefing_blocked": "secret_present"}).to_string()],
    )
    .expect("mark entity blocked");
    drop(conn);
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:demo.entry", "confidence": "ambiguous"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    let sites = resp["result"]["sites"].as_array().expect("sites array");
    assert!(!sites.is_empty(), "{resp:?}");
    for site in sites {
        assert_eq!(
            site["line_text"], "",
            "briefing-blocked owner must redact line_text: {site:?}"
        );
        assert_eq!(site["briefing_blocked"], true, "{site:?}");
    }
    // The blocked file's source bytes must not leak anywhere in the payload.
    let blob = resp.to_string();
    assert!(
        !blob.contains("return mid"),
        "leaked blocked source: {blob}"
    );
    assert!(
        !blob.contains("return target"),
        "leaked blocked source: {blob}"
    );
}

#[tokio::test]
async fn call_sites_kind_filter_limits_to_one_edge_kind() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:demo.entry", "kind": "references"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    let sites = resp["result"]["sites"].as_array().expect("sites");
    assert!(!sites.is_empty(), "{resp:?}");
    assert!(
        sites.iter().all(|s| s["edge_kind"] == "references"),
        "kind=references leaked other edge kinds: {resp:?}"
    );
}

#[tokio::test]
async fn call_sites_path_scope_filters_by_test_heuristic() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    // The seed lives in demo.py (not a conventional test path), so the unfiltered
    // and production scopes agree and the test scope filters everything out.
    let unfiltered = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:demo.entry", "confidence": "ambiguous"}),
    )
    .await;
    let production = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:demo.entry", "confidence": "ambiguous", "path": "production"}),
    )
    .await;
    let test = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:demo.entry", "confidence": "ambiguous", "path": "test"}),
    )
    .await;
    assert_eq!(production["ok"], true, "{production:?}");
    assert_eq!(test["ok"], true, "{test:?}");

    let n_unfiltered = unfiltered["result"]["sites"].as_array().unwrap().len();
    let n_production = production["result"]["sites"].as_array().unwrap().len();
    assert!(n_unfiltered > 0, "{unfiltered:?}");
    assert_eq!(
        n_production, n_unfiltered,
        "production scope should keep all demo.py sites"
    );
    assert!(
        test["result"]["sites"].as_array().unwrap().is_empty(),
        "test scope should exclude the non-test demo.py sites: {test:?}"
    );
}

#[tokio::test]
async fn call_sites_marks_unresolved_separately_and_rejects_unknown_entity() {
    let (project, db_path) = open_project();
    // Seed an unresolved (statically unbindable) call site for demo.entry.
    {
        let conn = Connection::open(&db_path).expect("open");
        insert_unresolved_call_site(
            &conn,
            "python:function:demo.entry",
            "site-dynamic",
            "ctx.handler.dispatch",
        );
    }
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    let unresolved = resp["result"]["unresolved_sites"]
        .as_array()
        .expect("unresolved_sites");
    assert_eq!(unresolved.len(), 1, "{resp:?}");
    assert_eq!(unresolved[0]["callee_expr"], "ctx.handler.dispatch");
    // Unresolved evidence must not be mixed into the resolved `sites` list.
    let resolved_exprs: Vec<&str> = resp["result"]["sites"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["callee_expr"].as_str())
        .collect();
    assert!(resolved_exprs.is_empty(), "{resp:?}");

    let unknown = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:does.not.exist"}),
    )
    .await;
    assert_eq!(unknown["ok"], false, "{unknown:?}");
    assert_eq!(unknown["error"]["code"], "not-found");
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
async fn find_entity_kind_filter_returns_only_that_kind() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    // Unfiltered "demo" search returns the module AND its functions.
    let unfiltered = call_tool(
        &state,
        "find_entity",
        json!({"pattern": "demo", "limit": 100}),
    )
    .await;
    assert_eq!(unfiltered["ok"], true);
    let kinds: std::collections::BTreeSet<String> = unfiltered["result"]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        kinds.contains("module") && kinds.contains("function"),
        "{kinds:?}"
    );

    // kind=module returns only modules.
    let modules = call_tool(
        &state,
        "find_entity",
        json!({"pattern": "demo", "limit": 100, "kind": "module"}),
    )
    .await;
    assert_eq!(modules["ok"], true);
    let module_entities = modules["result"]["entities"].as_array().unwrap();
    assert!(!module_entities.is_empty(), "{modules:?}");
    assert!(
        module_entities.iter().all(|e| e["kind"] == "module"),
        "kind filter leaked non-module entities: {modules:?}"
    );

    // A blank kind is a malformed request — it surfaces as a JSON-RPC error,
    // not a tool envelope, so drive handle_json_rpc directly here.
    let blank = state
        .handle_json_rpc(&json!({
            "jsonrpc": "2.0",
            "id": "blank-kind",
            "method": "tools/call",
            "params": {"name": "find_entity", "arguments": {"pattern": "demo", "kind": "  "}}
        }))
        .await
        .expect("response");
    assert!(
        blank["error"].is_object(),
        "blank kind should be a param error: {blank:?}"
    );
}

#[tokio::test]
async fn subsystem_of_resolves_module_and_contained_function() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    let subsystem_id = seed_subsystem(&conn, project.path());
    drop(conn);
    let state = state_for(project.path(), &db_path);

    // A module resolves directly.
    let from_module = call_tool(&state, "subsystem_of", json!({"id": "python:module:demo"})).await;
    assert_eq!(from_module["ok"], true);
    assert_eq!(from_module["result"]["subsystem"]["id"], subsystem_id);
    assert_eq!(from_module["result"]["via_module_id"], "python:module:demo");

    // A contained function resolves through its module ancestor.
    let from_fn = call_tool(
        &state,
        "subsystem_of",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(from_fn["ok"], true);
    assert_eq!(from_fn["result"]["subsystem"]["id"], subsystem_id);
    assert_eq!(
        from_fn["result"]["subsystem"]["name"],
        "Subsystem abc123def456"
    );
    assert_eq!(from_fn["result"]["via_module_id"], "python:module:demo");
}

#[tokio::test]
async fn subsystem_of_reports_null_subsystem_and_missing_entity() {
    // No seed_subsystem: the demo module exists but is in no subsystem.
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    // Entity exists but has no subsystem -> ok with subsystem: null (a fact,
    // distinguishable from a missing entity).
    let no_sub = call_tool(&state, "subsystem_of", json!({"id": "python:module:demo"})).await;
    assert_eq!(no_sub["ok"], true);
    assert!(no_sub["result"]["subsystem"].is_null(), "{no_sub:?}");
    assert!(no_sub["result"]["via_module_id"].is_null(), "{no_sub:?}");

    // Missing entity -> ok:false entity-not-found envelope.
    let missing = call_tool(
        &state,
        "subsystem_of",
        json!({"id": "python:function:does.not.exist"}),
    )
    .await;
    assert_ne!(missing["ok"], true, "{missing:?}");
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
    add_dynamic_source(project.path());
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
    // Inferred (LLM) dispatch attempts the attribute-receiver cases, so nothing
    // is excluded from the search (clarion-0d204a3f16).
    assert_eq!(envelope["result"]["scope_excludes"], json!([]));
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
async fn attribute_receiver_call_is_excluded_at_resolved_but_attempted_at_inferred() {
    // Attribute-receiver call `ctx.dynamic()` (callee_expr `ctx.dynamic`): the
    // static resolver cannot bind the `ctx` receiver, but the site IS recorded as
    // unresolved, so inferred (LLM) dispatch — which keys off the method name —
    // can recover it. This mirrors the motivating elspeth case `ctx.orchestrator
    // .resume()` (callee_expr `orchestrator.resume`, recovered when resolving a
    // target named `resume`). Resolved/ambiguous must FLAG the blind spot;
    // inferred must not, because it actually searches the category
    // (clarion-0d204a3f16).
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    add_dynamic_source(project.path());
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
        "site-attr",
        "ctx.dynamic",
    );
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnyInferredProvider::new(
        r#"{"edges":[{"site_key":"site-attr","target_id":"python:function:demo.dynamic","confidence":0.9,"rationale":"attribute receiver"}]}"#,
    ));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    // Resolved: the attribute-receiver caller is not bound, and the blind spot is flagged.
    let resolved = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.dynamic"}),
    )
    .await;
    assert_eq!(resolved["ok"], true);
    assert_eq!(resolved["result"]["callers"].as_array().unwrap().len(), 0);
    assert_eq!(
        resolved["result"]["scope_excludes"],
        json!(["attribute-receiver-calls"])
    );

    // Inferred: LLM dispatch recovers the attribute-receiver caller, so nothing is
    // excluded — the empty-vs-complete distinction is honest, not wallpaper.
    let inferred = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.dynamic", "confidence": "inferred"}),
    )
    .await;
    assert_eq!(inferred["ok"], true);
    assert_eq!(
        inferred["result"]["callers"][0]["entity"]["id"],
        "python:function:demo.entry"
    );
    assert_eq!(inferred["result"]["scope_excludes"], json!([]));

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callers_of_inferred_skips_briefing_blocked_callers_without_llm() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    add_dynamic_source(project.path());
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
    conn.execute(
        "UPDATE entities SET properties = ?1 WHERE id = 'python:function:demo.entry'",
        params![json!({"briefing_blocked": "secret_present"}).to_string()],
    )
    .expect("mark caller briefing-blocked");
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
    assert_eq!(envelope["result"]["callers"].as_array().unwrap().len(), 0);
    assert_eq!(
        envelope["stats_delta"]["inferred_dispatch_briefing_blocked_total"],
        1
    );
    assert!(provider.invocations().is_empty());

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inferred_dispatch_prompt_uses_caller_source_range_not_whole_file() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    add_dynamic_source(project.path());
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
    add_dynamic_source(project.path());
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
    add_dynamic_source(project.path());
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

// clarion-df58379de4: when the LLM hallucinates a `target_id` that isn't in the
// `entities` table, the inferred-dispatch path must drop that edge before
// reaching the writer-actor's FK-protected INSERT. The cache row must still be
// persisted so a warm rerun does not re-burn LLM tokens, and the dispatch must
// return `ok=true` with a `inferred_unresolved_targets_dropped_total` count.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callers_of_inferred_drops_targets_missing_from_entities_table() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    add_dynamic_source(project.path());
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
        r#"{"edges":[{"site_key":"site-dynamic","target_id":"python:function:nonexistent.hallucinated","confidence":0.91,"rationale":"name match"}]}"#,
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

    assert_eq!(envelope["ok"], true, "envelope was {envelope}");
    assert_eq!(
        envelope["stats_delta"]["inferred_unresolved_targets_dropped_total"],
        1
    );
    assert_eq!(
        envelope["stats_delta"]["inferred_edges_materialized_total"],
        0
    );
    assert_eq!(envelope["result"]["callers"].as_array().unwrap().len(), 0);
    assert_eq!(provider.invocations().len(), 1);

    // Warm rerun: cache row must have been persisted even though the LLM
    // proposed an unresolvable target, so we do not re-dispatch.
    let warm = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.dynamic", "confidence": "inferred"}),
    )
    .await;
    assert_eq!(warm["ok"], true);
    assert_eq!(provider.invocations().len(), 1, "cache miss on warm rerun");
    assert_eq!(
        warm["stats_delta"]["inferred_unresolved_targets_dropped_total"],
        1
    );

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

// ── compact ranked execution paths (clarion-5b3eff9a91 + clarion-23ae24358c) ──

#[tokio::test]
async fn execution_paths_from_returns_compact_node_table_and_id_paths() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "execution_paths_from",
        json!({"id": "python:function:demo.entry", "max_depth": 3}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    let result = &envelope["result"];
    assert_eq!(result["root"], "python:function:demo.entry");

    // paths are arrays of node-id strings, not re-serialized node objects.
    let paths = result["paths"].as_array().expect("paths array");
    assert!(!paths.is_empty());
    for path in paths {
        for node in path.as_array().unwrap() {
            assert!(
                node.is_string(),
                "path element must be a node-id string, got {node:?}"
            );
        }
    }

    // nodes is a deduplicated table: each id once, no content_hash bloat,
    // short_name retained for readability.
    let nodes = result["nodes"].as_array().expect("nodes array");
    let ids: Vec<&str> = nodes.iter().map(|n| n["id"].as_str().unwrap()).collect();
    let mut deduped = ids.clone();
    deduped.sort_unstable();
    deduped.dedup();
    assert_eq!(ids.len(), deduped.len(), "node table must be deduplicated");
    assert!(
        nodes.iter().all(|n| n.get("content_hash").is_none()),
        "compact nodes must drop content_hash"
    );
    assert!(nodes.iter().all(|n| n.get("short_name").is_some()));

    // every id referenced by a path resolves in the node table.
    let node_ids: std::collections::HashSet<&str> = ids.into_iter().collect();
    for path in paths {
        for node in path.as_array().unwrap() {
            assert!(
                node_ids.contains(node.as_str().unwrap()),
                "path references node {node:?} absent from the node table"
            );
        }
    }
}

#[tokio::test]
async fn execution_paths_from_ranks_longest_paths_first() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "execution_paths_from",
        json!({"id": "python:function:demo.entry", "max_depth": 3}),
    )
    .await;

    let paths = envelope["result"]["paths"].as_array().unwrap();
    let lengths: Vec<usize> = paths.iter().map(|p| p.as_array().unwrap().len()).collect();
    let mut descending = lengths.clone();
    descending.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(
        lengths, descending,
        "paths must be ranked longest-first, got {lengths:?}"
    );
}

#[tokio::test]
async fn execution_paths_from_path_cap_sets_truncated() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path).with_path_cap(1);

    let envelope = call_tool(
        &state,
        "execution_paths_from",
        json!({"id": "python:function:demo.entry", "max_depth": 3, "confidence": "ambiguous"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["truncated"], true);
    assert_eq!(envelope["truncation_reason"], "path-cap");
    assert_eq!(envelope["result"]["paths"].as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execution_paths_from_inferred_dispatches_start_caller() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    add_dynamic_source(project.path());
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
                    .any(|node_id| node_id == "python:function:demo.dynamic")
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
    add_dynamic_source(project.path());
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
                    .any(|node_id| node_id == "python:function:demo.dynamic")
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

#[tokio::test]
async fn neighborhood_surfaces_import_edges_for_reverse_import_lookup() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    // demo imports `other`; `client` imports demo. The reverse-import question
    // ("who imports demo?") is answerable only if neighborhood surfaces the
    // distinct `imports` edge kind, not just `references` (clarion-79d0ff6e14).
    std::fs::write(project.path().join("other.py"), "x = 1\n").unwrap();
    std::fs::write(project.path().join("client.py"), "import demo\n").unwrap();
    insert_entity(
        &conn,
        "python:module:other",
        "module",
        &project.path().join("other.py"),
        Some((1, 1)),
        None,
    );
    insert_entity(
        &conn,
        "python:module:client",
        "module",
        &project.path().join("client.py"),
        Some((1, 1)),
        None,
    );
    insert_edge(
        &conn,
        "imports",
        "python:module:demo",
        "python:module:other",
        "resolved",
        None,
    );
    insert_edge(
        &conn,
        "imports",
        "python:module:client",
        "python:module:demo",
        "resolved",
        None,
    );
    drop(conn);

    let state = state_for(project.path(), &db_path);
    let envelope = call_tool(&state, "neighborhood", json!({"id": "python:module:demo"})).await;

    assert_eq!(envelope["ok"], true, "{envelope}");
    let imports_out: Vec<&str> = envelope["result"]["imports_out"]
        .as_array()
        .expect("imports_out array")
        .iter()
        .map(|n| n["entity"]["id"].as_str().unwrap())
        .collect();
    assert!(
        imports_out.contains(&"python:module:other"),
        "imports_out should list what demo imports: {envelope}"
    );
    let imports_in: Vec<&str> = envelope["result"]["imports_in"]
        .as_array()
        .expect("imports_in array")
        .iter()
        .map(|n| n["entity"]["id"].as_str().unwrap())
        .collect();
    assert!(
        imports_in.contains(&"python:module:client"),
        "imports_in should list who imports demo: {envelope}"
    );
}

// ── scope_excludes on graph-query results (clarion-0d204a3f16) ───────────────

#[tokio::test]
async fn callers_of_resolved_flags_attribute_receiver_scope_exclusion() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.target"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(
        envelope["result"]["scope_excludes"],
        json!(["attribute-receiver-calls"])
    );
}

#[tokio::test]
async fn execution_paths_from_resolved_flags_attribute_receiver_scope_exclusion() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "execution_paths_from",
        json!({"id": "python:function:demo.entry", "confidence": "ambiguous"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(
        envelope["result"]["scope_excludes"],
        json!(["attribute-receiver-calls"])
    );
}

#[tokio::test]
async fn neighborhood_module_rolls_up_references_and_flags_attribute_scope() {
    // The module now rolls up contained symbols' reference edges instead of
    // flagging the rollup as a blind spot (clarion-79d0ff6e14). The seeded
    // graph's only `references` edge (entry -> target) is intra-module, so it
    // is correctly excluded — references_in/out are empty but the response
    // signals the rollup happened.
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:module:demo", "confidence": "resolved"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(
        envelope["result"]["references_rolled_up"], true,
        "module neighborhood must signal references are rolled up to module altitude"
    );
    let excludes = envelope["result"]["scope_excludes"]
        .as_array()
        .expect("scope_excludes array");
    assert!(
        excludes.iter().any(|v| v == "attribute-receiver-calls"),
        "module neighborhood must flag attribute-receiver-calls, got {excludes:?}"
    );
    assert!(
        !excludes
            .iter()
            .any(|v| v == "module-level-reference-rollup"),
        "module rollup is now implemented; the blind-spot marker must be gone, got {excludes:?}"
    );
    assert_eq!(
        envelope["result"]["references_in"],
        json!([]),
        "the only seeded reference is intra-module and must be excluded from the rollup"
    );
}

#[tokio::test]
async fn neighborhood_function_references_are_not_rolled_up() {
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
        envelope["result"]["references_rolled_up"], false,
        "symbol-level references are direct, not rolled up"
    );
    assert_eq!(
        envelope["result"]["scope_excludes"],
        json!(["attribute-receiver-calls"])
    );
}

#[tokio::test]
async fn neighborhood_module_rollup_surfaces_external_reverse_import() {
    // Seed an external module symbol that references a symbol contained in
    // `demo`, then confirm the module-altitude rollup answers "who imports this
    // module / contract?" with the referencer tagged by the `via` symbol
    // (clarion-79d0ff6e14).
    let (project, db_path) = open_project();
    {
        let conn = Connection::open(&db_path).expect("reopen db");
        let source_path = project.path().join("consumer.py");
        std::fs::write(
            &source_path,
            "import demo\n\ndef use():\n    return demo.target()\n",
        )
        .expect("write consumer source");
        insert_entity(
            &conn,
            "python:module:consumer",
            "module",
            &source_path,
            Some((1, 4)),
            None,
        );
        insert_entity(
            &conn,
            "python:function:consumer.use",
            "function",
            &source_path,
            Some((3, 4)),
            Some("python:module:consumer"),
        );
        insert_edge(
            &conn,
            "contains",
            "python:module:consumer",
            "python:function:consumer.use",
            "resolved",
            None,
        );
        insert_edge(
            &conn,
            "references",
            "python:function:consumer.use",
            "python:function:demo.target",
            "resolved",
            None,
        );
    }
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:module:demo", "confidence": "resolved"}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["references_rolled_up"], true);
    let refs_in = envelope["result"]["references_in"]
        .as_array()
        .expect("references_in array");
    assert_eq!(
        refs_in.len(),
        1,
        "external referencer must roll up: {refs_in:?}"
    );
    assert_eq!(
        refs_in[0]["entity"]["id"], "python:function:consumer.use",
        "neighbor is the external referencer (who imports)"
    );
    assert_eq!(
        refs_in[0]["via"]["id"], "python:function:demo.target",
        "via names the contained symbol the import touched"
    );
    // Reverse-import names importing MODULES, not just symbols
    // (clarion-79d0ff6e14 AC): the importing symbol's containing module is
    // surfaced alongside the symbol, so "who imports this" is answerable at
    // module altitude.
    assert_eq!(
        refs_in[0]["importer_module"]["id"], "python:module:consumer",
        "importer_module rolls the importing symbol up to its module"
    );
}

#[tokio::test]
async fn index_diff_is_reachable_over_mcp_and_reports_freshness() {
    // AC: index_diff output is available over MCP. A completed run dated far in
    // the future keeps the just-written demo.py un-modified, so the verdict is
    // deterministic regardless of the (non-repo) tempdir's git environment.
    let (project, db_path) = open_project();
    {
        let conn = Connection::open(&db_path).expect("reopen db");
        insert_run(
            &conn,
            "run-fresh",
            "2999-01-01T00:00:00.000Z",
            "completed",
            Some("2999-01-01T00:00:05.000Z"),
        );
    }
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(&state, "index_diff", json!({})).await;

    assert_eq!(envelope["ok"], true);
    let result = &envelope["result"];
    assert_eq!(result["overall"], "fresh");
    assert_eq!(result["drift_detected"], false);
    // analyzed_commit is null by design; the git block is always present.
    assert_eq!(result["analyzed_commit"], Value::Null);
    assert!(result["git"]["available"].is_boolean());
    assert_eq!(result["analyzed_at"], "2999-01-01T00:00:05.000Z");
    assert_eq!(
        result["indexed_files"], 1,
        "the seeded graph has a single source file (demo.py)"
    );
    assert_eq!(result["modified_since_analyze"], json!([]));
}

#[tokio::test]
async fn index_diff_reports_never_analyzed_without_a_completed_run() {
    // open_project seeds entities but no run row.
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(&state, "index_diff", json!({})).await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["overall"], "never_analyzed");
    assert_eq!(envelope["result"]["drift_detected"], false);
}

// ── project_status diagnostics tool (clarion-084e82250c) ─────────────────────

fn insert_run(
    conn: &Connection,
    id: &str,
    started_at: &str,
    status: &str,
    completed: Option<&str>,
) {
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES (?1, ?2, ?3, '{}', '{}', ?4)",
        params![id, started_at, completed, status],
    )
    .expect("insert run");
}

fn insert_finding(conn: &Connection, id: &str, run_id: &str, entity_id: &str) {
    conn.execute(
        "INSERT INTO findings \
         (id, tool, tool_version, run_id, rule_id, kind, severity, entity_id, \
          related_entities, message, evidence, properties, supports, supported_by, \
          status, created_at, updated_at) \
         VALUES (?1,'clarion','1.0',?2,'R1','defect','WARN',?3,'[]','m','{}','{}','[]','[]', \
                 'open','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
        params![id, run_id, entity_id],
    )
    .expect("insert finding");
}

#[test]
fn tools_list_includes_project_status() {
    let tools = list_tools();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "project_status")
        .expect("project_status tool definition");
    assert_eq!(
        tool.input_schema,
        json!({"type": "object", "properties": {}, "additionalProperties": false})
    );
}

#[tokio::test]
async fn project_status_reports_db_identity_for_drift_detection() {
    // A swapped/stale DB is otherwise invisible to a consult agent. Report a
    // db_identity block (on-disk size + SQLite data_version) so drift is
    // detectable across calls (clarion-22c18fdb34).
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(&state, "project_status", json!({})).await;

    assert_eq!(envelope["ok"], true, "{envelope}");
    let identity = &envelope["result"]["db_identity"];
    assert!(
        identity["db_size_bytes"].as_u64().is_some_and(|n| n > 0),
        "db_size_bytes should reflect the served file: {envelope}"
    );
    assert!(
        identity["data_version"].as_i64().is_some(),
        "data_version should be reported for drift detection: {envelope}"
    );
}

#[tokio::test]
async fn project_status_reports_counts_latest_run_and_plugins() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    seed_subsystem(&conn, project.path());
    insert_run(
        &conn,
        "run-1",
        "2026-02-02T00:00:00.000Z",
        "completed",
        Some("2026-02-02T00:00:00.000Z"),
    );
    insert_finding(&conn, "f-1", "run-1", "python:function:demo.entry");
    // One entity withheld from briefings (secret scan set briefing_blocked).
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, properties, \
         created_at, updated_at) \
         VALUES (?1, 'python', 'function', ?1, ?1, ?2, \
         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
        rusqlite::params![
            "python:function:demo.secret",
            r#"{"briefing_blocked": "secret_detected"}"#
        ],
    )
    .expect("insert briefing-blocked entity");
    drop(conn);

    let state = state_for(project.path(), &db_path);
    let envelope = call_tool(&state, "project_status", json!({})).await;
    assert_eq!(envelope["ok"], true);
    let result = &envelope["result"];

    assert!(result["counts"]["entities"].as_i64().unwrap() >= 4);
    assert_eq!(result["counts"]["subsystems"], 1);
    assert!(result["counts"]["edges"].as_i64().unwrap() >= 1);
    assert_eq!(result["counts"]["findings"], 1);
    // The briefing_blocked count is served by the partial index over the
    // generated column (clarion-bdabfd6bca).
    assert_eq!(result["counts"]["briefing_blocked"], 1);

    // AC#1: latest completed run + counts.
    assert_eq!(result["latest_run"]["id"], "run-1");
    assert_eq!(result["latest_run"]["status"], "completed");
    assert_eq!(
        result["latest_run"]["completed_at"],
        "2026-02-02T00:00:00.000Z"
    );
    assert_eq!(result["last_analyzed_at"], "2026-02-02T00:00:00.000Z");

    let plugin_ids: Vec<&str> = result["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|plugin| plugin["plugin_id"].as_str().unwrap())
        .collect();
    assert!(plugin_ids.contains(&"python"), "plugins: {plugin_ids:?}");

    assert!(
        result["db_path"]
            .as_str()
            .unwrap()
            .ends_with(".clarion/clarion.db")
    );
    // No analyze-time git SHA is persisted; reported as null, not fabricated.
    assert_eq!(result["git_sha"], Value::Null);
    // A bare ServerState carries no diagnostics context.
    assert_eq!(result["llm"], Value::Null);
    assert_eq!(result["filigree"], Value::Null);
}

#[tokio::test]
async fn project_status_marks_skipped_no_plugins_run() {
    // AC#2: a skipped_no_plugins run is unmistakable as no index refresh.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    insert_run(
        &conn,
        "run-skip",
        "2026-02-03T00:00:00.000Z",
        "skipped_no_plugins",
        Some("2026-02-03T00:00:00.000Z"),
    );
    drop(conn);
    let state = state_for(project.path(), &db_path);
    let envelope = call_tool(&state, "project_status", json!({})).await;
    assert_eq!(
        envelope["result"]["latest_run"]["status"],
        "skipped_no_plugins"
    );
}

#[tokio::test]
async fn project_status_skipped_run_keeps_prior_completed_index_visible() {
    // The real dogfood shape: a skipped_no_plugins run AFTER a completed one.
    // latest_run.status flags the skip, while last_analyzed_at + counts still
    // describe the older, usable index ("your last attempt skipped — here's the
    // index from before").
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    insert_run(
        &conn,
        "run-old",
        "2026-01-01T00:00:00.000Z",
        "completed",
        Some("2026-01-15T00:00:00.000Z"),
    );
    insert_run(
        &conn,
        "run-skip",
        "2026-02-01T00:00:00.000Z",
        "skipped_no_plugins",
        Some("2026-02-01T00:00:00.000Z"),
    );
    drop(conn);
    let state = state_for(project.path(), &db_path);
    let result = call_tool(&state, "project_status", json!({})).await["result"].clone();

    assert_eq!(result["latest_run"]["id"], "run-skip");
    assert_eq!(result["latest_run"]["status"], "skipped_no_plugins");
    // last_analyzed_at tracks the latest *completed* run, not the skip.
    assert_eq!(result["last_analyzed_at"], "2026-01-15T00:00:00.000Z");
    assert!(result["counts"]["entities"].as_i64().unwrap() >= 3);
}

#[tokio::test]
async fn project_status_resolves_live_filigree_endpoint() {
    // AC#3: the live ethereal port (.filigree/ephemeral.port) is reported as
    // the resolution source, overriding the stale configured port.
    let (project, db_path) = open_project();
    let filigree_dir = project.path().join(".filigree");
    fs::create_dir_all(&filigree_dir).unwrap();
    fs::write(filigree_dir.join("ephemeral.port"), "8542").unwrap();

    let config = FiligreeConfig {
        enabled: true,
        ..FiligreeConfig::default()
    };
    let diagnostics = DiagnosticsContext {
        llm: LlmDiagnostics {
            provider: "disabled".to_owned(),
            live: false,
            allow_live_provider: false,
            cache_max_age_days: 180,
        },
        filigree: resolve_filigree_url(&config, project.path()),
    };
    let state = state_for(project.path(), &db_path).with_diagnostics(diagnostics);

    let envelope = call_tool(&state, "project_status", json!({})).await;
    let filigree = &envelope["result"]["filigree"];
    assert_eq!(filigree["enabled"], true);
    assert_eq!(filigree["configured_url"], "http://127.0.0.1:8766");
    assert_eq!(filigree["resolved_url"], "http://127.0.0.1:8542");
    assert_eq!(filigree["resolution_source"], SOURCE_EPHEMERAL_PORT);

    let llm = &envelope["result"]["llm"];
    assert_eq!(llm["provider"], "disabled");
    assert_eq!(llm["live"], false);
    assert_eq!(llm["cache_max_age_days"], 180);
}

#[tokio::test]
async fn project_status_filigree_falls_back_to_config_without_port_file() {
    let (project, db_path) = open_project();
    let config = FiligreeConfig {
        enabled: true,
        ..FiligreeConfig::default()
    };
    let diagnostics = DiagnosticsContext {
        llm: LlmDiagnostics {
            provider: "openrouter".to_owned(),
            live: true,
            allow_live_provider: true,
            cache_max_age_days: 7,
        },
        filigree: resolve_filigree_url(&config, project.path()),
    };
    let state = state_for(project.path(), &db_path).with_diagnostics(diagnostics);
    let envelope = call_tool(&state, "project_status", json!({})).await;
    let filigree = &envelope["result"]["filigree"];
    assert_eq!(filigree["resolved_url"], "http://127.0.0.1:8766");
    assert_eq!(filigree["resolution_source"], SOURCE_CONFIG);
    assert_eq!(envelope["result"]["llm"]["live"], true);
}

// ---------------------------------------------------------------------------
// Wardline Flow B helpers and tests
// ---------------------------------------------------------------------------

/// Build a `WardlineFinding` with `metadata.wardline.qualname` set — models a
/// finding that can be reconciled to a named entity. The wardline block carries
/// only `qualname`; use `wf_full` when you need the full block for assertions.
fn wf(qualname: &str, rule_id: &str) -> WardlineFinding {
    WardlineFinding {
        rule_id: rule_id.to_owned(),
        message: format!("taint finding for {qualname}"),
        severity: Some("high".to_owned()),
        status: Some("open".to_owned()),
        line_start: Some(10),
        line_end: Some(12),
        fingerprint: Some(format!("fp-{rule_id}")),
        file_id: Some("file-test".to_owned()),
        metadata: serde_json::json!({ "wardline": { "qualname": qualname } }),
    }
}

/// Build a `WardlineFinding` with a full `metadata.wardline` block —
/// kind/confidence/suppression/qualname — for assertions that the wardline
/// sub-object is surfaced verbatim in section items.
fn wf_full(qualname: &str, rule_id: &str) -> WardlineFinding {
    WardlineFinding {
        rule_id: rule_id.to_owned(),
        message: format!("taint finding for {qualname}"),
        severity: Some("high".to_owned()),
        status: Some("open".to_owned()),
        line_start: Some(10),
        line_end: Some(12),
        fingerprint: Some(format!("fp-{rule_id}")),
        file_id: Some("file-test".to_owned()),
        metadata: serde_json::json!({
            "wardline": {
                "qualname": qualname,
                "kind": "taint",
                "confidence": "high",
                "suppression": null
            }
        }),
    }
}

/// Build a `WardlineFinding` without a qualname in metadata — counted as
/// `omitted_no_qualname` by the reconciler.
fn wf_no_qualname(rule_id: &str) -> WardlineFinding {
    WardlineFinding {
        rule_id: rule_id.to_owned(),
        message: "metric finding without qualname".to_owned(),
        severity: Some("info".to_owned()),
        status: Some("open".to_owned()),
        line_start: None,
        line_end: None,
        fingerprint: None,
        file_id: Some("file-test".to_owned()),
        metadata: serde_json::json!({ "wardline": { "kind": "METRIC" } }),
    }
}

#[tokio::test]
async fn issues_for_attaches_exact_wardline_findings() {
    // AC: `wardline_findings` section is attached to the `issues_for` result for
    // the requested entity. Exact-match finding is included; a finding for a
    // different qualname is not; a finding with no qualname is omitted and
    // counted.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    // Insert the entity under test with a known source_file_path. Use a raw
    // INSERT because we only need the row to exist for issues_for dispatch; we
    // do not need a correct content_hash for the wardline section itself.
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash,
            created_at, updated_at
         ) VALUES (
            'python:function:demo.hello', 'python', 'function',
            'python:function:demo.hello', 'demo.hello', NULL,
            'src/demo.py', 1, 3, '{}', 'fake-hash-wf-test',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [],
    )
    .expect("insert demo.hello entity");
    drop(conn);

    let client = Arc::new(FakeFiligreeClient::default().with_wardline_findings(vec![
        wf("demo.hello", "WLN-TAINT-001"), // exact match -> attached
        wf("demo.other", "WLN-TAINT-002"), // different entity -> NOT attached
        wf_no_qualname("WLN-METRIC-001"),  // no qualname -> omitted
    ]));
    let state = state_for_filigree(project.path(), &db_path, client);

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.hello", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    let section = &envelope["result"]["wardline_findings"];
    assert_eq!(section["result_kind"], "matched", "section: {section}");
    let items = section["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "only the exact-match finding is included");
    assert_eq!(items[0]["rule_id"], "WLN-TAINT-001");
    assert_eq!(items[0]["resolution_confidence"], "exact");
    assert_eq!(
        section["omitted_no_qualname"], 1,
        "one finding had no qualname"
    );
}

#[tokio::test]
async fn issues_for_degrades_when_wardline_fetch_errors() {
    // AC: when `wardline_findings_for_path` returns an error, the section
    // degrades to `result_kind: "unavailable"` and items is empty — the tool
    // itself still succeeds.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash,
            created_at, updated_at
         ) VALUES (
            'python:function:demo.hello', 'python', 'function',
            'python:function:demo.hello', 'demo.hello', NULL,
            'src/demo.py', 1, 3, '{}', 'fake-hash-wf-test',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [],
    )
    .expect("insert demo.hello entity");
    drop(conn);

    let client = Arc::new(FakeFiligreeClient::default().with_wardline_error());
    let state = state_for_filigree(project.path(), &db_path, client);

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.hello", "include_contained": false}),
    )
    .await;

    assert_eq!(
        envelope["ok"], true,
        "tool must succeed even on wardline error"
    );
    let section = &envelope["result"]["wardline_findings"];
    assert_eq!(
        section["result_kind"], "unavailable",
        "section degrades on error: {section}"
    );
    let items = section["items"].as_array().expect("items array");
    assert!(items.is_empty(), "no items when unavailable");
}

#[tokio::test]
async fn issues_for_wardline_no_matches_when_no_qualname_reconciles() {
    // AC (no-fabrication axiom): when Filigree returns findings but NONE
    // reconcile to the requested entity (they target a different qualname), the
    // section is `no_matches` with an empty items array — Clarion never invents
    // a match. The findings have qualnames, so omitted_no_qualname stays 0.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash,
            created_at, updated_at
         ) VALUES (
            'python:function:demo.hello', 'python', 'function',
            'python:function:demo.hello', 'demo.hello', NULL,
            'src/demo.py', 1, 3, '{}', 'fake-hash-wf-test',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [],
    )
    .expect("insert demo.hello entity");
    drop(conn);

    let client = Arc::new(
        FakeFiligreeClient::default()
            .with_wardline_findings(vec![wf("demo.other", "WLN-TAINT-999")]),
    );
    let state = state_for_filigree(project.path(), &db_path, client);

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.hello", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    let section = &envelope["result"]["wardline_findings"];
    assert_eq!(
        section["result_kind"], "no_matches",
        "no finding reconciles to the entity: {section}"
    );
    let items = section["items"].as_array().expect("items array");
    assert!(items.is_empty(), "no fabricated matches");
    assert_eq!(
        section["omitted_no_qualname"], 0,
        "the finding had a qualname, just a non-matching one"
    );
}

#[tokio::test]
async fn orientation_pack_includes_wardline_findings() {
    // AC: `wardline_findings` is lifted out of the delegated `issues_for` result
    // and surfaced as a top-level section of the orientation pack — no second
    // Filigree fetch, no direct client call. The section must NOT appear inside
    // `issues` (it was removed from there).
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash,
            created_at, updated_at
         ) VALUES (
            'python:function:demo.hello', 'python', 'function',
            'python:function:demo.hello', 'demo.hello', NULL,
            'src/demo.py', 1, 3, '{}', 'fake-hash-orient-wf',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [],
    )
    .expect("insert demo.hello entity");
    drop(conn);

    let client = Arc::new(
        FakeFiligreeClient::default()
            .with_wardline_findings(vec![wf("demo.hello", "WLN-TAINT-001")]),
    );
    let state = state_for_filigree(project.path(), &db_path, client);

    let out = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.hello"}),
    )
    .await;

    assert_eq!(out["ok"], true, "{out:?}");

    // wardline_findings is a top-level section of the pack (sibling of issues,
    // health, etc.), under out["result"] because orientation_pack uses
    // success_envelope_with_truncation.
    let wf_section = &out["result"]["wardline_findings"];
    assert_eq!(
        wf_section["result_kind"], "matched",
        "wardline_findings section: {wf_section}"
    );
    let items = wf_section["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "one matched finding");
    assert_eq!(items[0]["rule_id"], "WLN-TAINT-001");

    // The section must NOT be duplicated inside issues (it was lifted out).
    assert!(
        out["result"]["issues"].get("wardline_findings").is_none(),
        "wardline_findings must not appear inside the issues section"
    );
}

// ---------------------------------------------------------------------------
// FIX 1: metadata.wardline block surfaced in items
// ---------------------------------------------------------------------------

#[tokio::test]
async fn issues_for_section_item_carries_wardline_metadata_block() {
    // AC (FIX 1): each item in `wardline_findings.items` includes a `wardline`
    // key with the full wardline sub-object from `metadata.wardline` —
    // kind, confidence, suppression, qualname. The block is passed through
    // verbatim; Clarion does not selectively strip or rename fields.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash,
            created_at, updated_at
         ) VALUES (
            'python:function:demo.hello', 'python', 'function',
            'python:function:demo.hello', 'demo.hello', NULL,
            'src/demo.py', 1, 3, '{}', 'fake-hash-wf-meta',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [],
    )
    .expect("insert demo.hello entity");
    drop(conn);

    let client = Arc::new(
        FakeFiligreeClient::default()
            .with_wardline_findings(vec![wf_full("demo.hello", "WLN-TAINT-001")]),
    );
    let state = state_for_filigree(project.path(), &db_path, client);

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.hello", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    let section = &envelope["result"]["wardline_findings"];
    assert_eq!(section["result_kind"], "matched", "section: {section}");
    let items = section["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "one matched finding");
    // The wardline sub-object must be present and carry kind/confidence/suppression.
    let wb = &items[0]["wardline"];
    assert_eq!(wb["kind"], "taint", "wardline.kind in item: {wb}");
    assert_eq!(
        wb["confidence"], "high",
        "wardline.confidence in item: {wb}"
    );
    assert_eq!(
        wb["suppression"],
        Value::Null,
        "wardline.suppression null: {wb}"
    );
    assert_eq!(
        wb["qualname"], "demo.hello",
        "wardline.qualname in item: {wb}"
    );
}

// ---------------------------------------------------------------------------
// FIX 4: coverage gaps
// ---------------------------------------------------------------------------

#[tokio::test]
async fn issues_for_no_path_entity_returns_no_matches_without_fetch() {
    // AC (FIX 4a): entity with source_file_path IS NULL → section is
    // `no_matches` (empty) and the wardline client route is NOT invoked.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    // Insert an entity with source_file_path NULL.
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash,
            created_at, updated_at
         ) VALUES (
            'python:function:demo.nopath', 'python', 'function',
            'python:function:demo.nopath', 'demo.nopath', NULL,
            NULL, NULL, NULL, '{}', 'fake-hash-nopath',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [],
    )
    .expect("insert demo.nopath entity");
    drop(conn);

    // Client has findings — these must NOT be fetched for a no-path entity.
    let client = Arc::new(
        FakeFiligreeClient::default()
            .with_wardline_findings(vec![wf("demo.nopath", "WLN-TAINT-777")]),
    );
    let state = state_for_filigree(project.path(), &db_path, client);

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.nopath", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    let section = &envelope["result"]["wardline_findings"];
    assert_eq!(
        section["result_kind"], "no_matches",
        "no-path entity must produce no_matches: {section}"
    );
    let items = section["items"].as_array().expect("items array");
    assert!(items.is_empty(), "no items for no-path entity");
}

#[tokio::test]
async fn issues_for_multiple_matching_findings_all_included() {
    // AC (FIX 4b): when two findings both match the entity's qualname (two
    // different rule_ids on the same entity), both are surfaced in items.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash,
            created_at, updated_at
         ) VALUES (
            'python:function:demo.hello', 'python', 'function',
            'python:function:demo.hello', 'demo.hello', NULL,
            'src/demo.py', 1, 3, '{}', 'fake-hash-multi',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [],
    )
    .expect("insert demo.hello entity");
    drop(conn);

    let client = Arc::new(FakeFiligreeClient::default().with_wardline_findings(vec![
        wf("demo.hello", "WLN-TAINT-001"),
        wf("demo.hello", "WLN-TAINT-002"),
    ]));
    let state = state_for_filigree(project.path(), &db_path, client);

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.hello", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    let section = &envelope["result"]["wardline_findings"];
    assert_eq!(section["result_kind"], "matched", "section: {section}");
    let items = section["items"].as_array().expect("items array");
    assert_eq!(items.len(), 2, "both findings must be surfaced: {section}");
    let rule_ids: Vec<&str> = items
        .iter()
        .map(|it| it["rule_id"].as_str().expect("rule_id string"))
        .collect();
    assert!(
        rule_ids.contains(&"WLN-TAINT-001"),
        "WLN-TAINT-001 missing: {rule_ids:?}"
    );
    assert!(
        rule_ids.contains(&"WLN-TAINT-002"),
        "WLN-TAINT-002 missing: {rule_ids:?}"
    );
}

#[tokio::test]
async fn issues_for_dotted_method_qualname_reconciles_end_to_end() {
    // AC (FIX 4c): entity `python:function:demo.Foo.bar` at src/demo.py; the
    // client returns a finding with qualname "demo.Foo.bar". The section must
    // be `matched` and the item present — proving dotted method qualnames
    // reconcile through the full dispatch path (spec §6).
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash,
            created_at, updated_at
         ) VALUES (
            'python:function:demo.Foo.bar', 'python', 'function',
            'python:function:demo.Foo.bar', 'demo.Foo.bar', NULL,
            'src/demo.py', 10, 12, '{}', 'fake-hash-foo-bar',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [],
    )
    .expect("insert demo.Foo.bar entity");
    drop(conn);

    let client = Arc::new(
        FakeFiligreeClient::default().with_wardline_findings(vec![wf("demo.Foo.bar", "WLN-X")]),
    );
    let state = state_for_filigree(project.path(), &db_path, client);

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.Foo.bar", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    let section = &envelope["result"]["wardline_findings"];
    assert_eq!(
        section["result_kind"], "matched",
        "dotted method qualname must reconcile: {section}"
    );
    let items = section["items"].as_array().expect("items array");
    assert_eq!(items.len(), 1, "one matched finding: {section}");
    assert_eq!(items[0]["rule_id"], "WLN-X");
}
