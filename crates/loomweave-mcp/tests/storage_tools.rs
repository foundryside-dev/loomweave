//! MCP storage-backed tool tests.

use std::{
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use loomweave_core::{
    CachingModel, INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput,
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID, LeafSummaryPromptInput, LlmProvider, LlmProviderError,
    LlmPurpose, LlmRequest, LlmResponse, OpenRouterProvider, OpenRouterProviderConfig, Recording,
    RecordingProvider, build_inferred_calls_prompt, build_leaf_summary_prompt,
};
use loomweave_federation::{
    loomweave_port::publish_port,
    loomweave_url::{
        SOURCE_EPHEMERAL_PORT as LOOMWEAVE_SOURCE_EPHEMERAL_PORT,
        SOURCE_NONE as LOOMWEAVE_SOURCE_NONE,
    },
};
use loomweave_mcp::{
    DiagnosticsContext, LlmDiagnostics, McpToolPolicy, ServerState,
    config::{FiligreeConfig, LlmConfig, LlmProviderKind},
    filigree::{
        EntityAssociation, EntityAssociationsResponse, FiligreeClientError, FiligreeLookup,
        IssueDetail, ObservationCreateRequest, ObservationCreateResponse, ObservationRecord,
        WardlineFinding,
    },
    filigree_url::{SOURCE_CONFIG, SOURCE_EPHEMERAL_PORT, resolve_filigree_url},
    list_tools,
};
use loomweave_storage::{
    GuidanceProposal, GuidanceSheetInput, ReaderPool, SummaryCacheEntry, SummaryCacheKey,
    TaintFact, Writer, pragma, schema, upsert_guidance_sheet, upsert_summary_cache,
    upsert_taint_fact,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};

fn open_project() -> (tempfile::TempDir, std::path::PathBuf) {
    let project = tempfile::tempdir().expect("temp project");
    let loomweave_dir = project.path().join(".weft/loomweave");
    std::fs::create_dir_all(&loomweave_dir).expect("create .loomweave");
    let db_path = loomweave_dir.join("loomweave.db");
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

    insert_file_entity(conn, "core:file:demo.py", &source_path);
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

fn insert_file_entity(conn: &Connection, id: &str, source_path: &std::path::Path) {
    let content_hash = blake3::hash(&std::fs::read(source_path).expect("read file source"))
        .to_hex()
        .to_string();
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, source_file_path, properties, content_hash,
            created_at, updated_at
         ) VALUES (
            ?1, 'core', 'file', ?2, ?2, ?3, '{}', ?4,
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        params![
            id,
            source_path.file_name().unwrap().to_string_lossy().as_ref(),
            source_path.display().to_string(),
            content_hash,
        ],
    )
    .expect("insert file entity");
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
         ) VALUES (?1, ?2, ?3, 0, 'core:file:demo.py', 30, 37, ?4, '2026-05-17T00:00:00.000Z')",
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
        .with_tool_policy(McpToolPolicy::allow_write_tools())
        .with_summary_llm(writer.sender(), config, provider)
        .with_clock(|| "2026-05-17T00:00:02.000Z".to_owned())
}

fn state_for_filigree(
    project_root: &std::path::Path,
    db_path: &std::path::Path,
    client: Arc<dyn FiligreeLookup>,
) -> ServerState {
    let pool = ReaderPool::open(db_path, 2).expect("reader pool");
    ServerState::new(project_root.to_path_buf(), pool)
        .with_tool_policy(McpToolPolicy::allow_write_tools())
        .with_filigree_client(client)
}

fn expected_summary_request(project_root: &std::path::Path, entity_id: &str) -> LlmRequest {
    let source_excerpt = expected_source_excerpt(project_root, entity_id);
    let prompt = build_leaf_summary_prompt(&LeafSummaryPromptInput {
        entity_id: entity_id.to_owned(),
        kind: "function".to_owned(),
        name: entity_id.to_owned(),
        guidance: String::new(),
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
        "source_file_id": "core:file:demo.py",
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

    fn invocations(&self) -> Vec<LlmRequest> {
        self.invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[derive(Debug)]
struct GatedInferredProvider {
    invocations: Mutex<Vec<LlmRequest>>,
    output_json: String,
    started: AtomicBool,
    started_notify: tokio::sync::Notify,
    release_notify: tokio::sync::Notify,
}

impl GatedInferredProvider {
    fn new(output_json: &str) -> Self {
        Self {
            invocations: Mutex::new(Vec::new()),
            output_json: output_json.to_owned(),
            started: AtomicBool::new(false),
            started_notify: tokio::sync::Notify::new(),
            release_notify: tokio::sync::Notify::new(),
        }
    }

    async fn wait_started(&self) {
        while !self.started.load(Ordering::SeqCst) {
            self.started_notify.notified().await;
        }
    }

    fn release(&self) {
        self.release_notify.notify_waiters();
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

#[async_trait::async_trait]
impl LlmProvider for AnySummaryProvider {
    fn name(&self) -> &'static str {
        "recording"
    }

    async fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
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

#[async_trait::async_trait]
impl LlmProvider for AnyInferredProvider {
    fn name(&self) -> &'static str {
        "recording"
    }

    async fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
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

#[async_trait::async_trait]
impl LlmProvider for GatedInferredProvider {
    fn name(&self) -> &'static str {
        "recording"
    }

    async fn invoke(&self, request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
        self.invocations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request.clone());
        self.started.store(true, Ordering::SeqCst);
        self.started_notify.notify_waiters();
        self.release_notify.notified().await;
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
    /// Paths requested through `wardline_findings_for_path`.
    wardline_path_calls: Mutex<Vec<String>>,
    /// When true, `wardline_findings_for_path` returns an `HttpStatus` 503 error.
    wardline_error: Mutex<bool>,
    /// When true, `issue_detail` returns an `HttpStatus` 503 error (the
    /// dogfood-4 B9 degrade path: transport/auth failure mid-enrichment).
    detail_error: Mutex<bool>,
    created_observations: Mutex<Vec<ObservationCreateRequest>>,
    observations: Mutex<std::collections::HashMap<String, ObservationRecord>>,
    dismissed_observations: Mutex<Vec<String>>,
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
                id: issue_id.to_owned(),
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

    fn with_detail_error(mut self) -> Self {
        *self.detail_error.get_mut().unwrap() = true;
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

    fn created_observations(&self) -> Vec<ObservationCreateRequest> {
        self.created_observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn wardline_path_calls(&self) -> Vec<String> {
        self.wardline_path_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn dismissed_observations(&self) -> Vec<String> {
        self.dismissed_observations
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
        if *self
            .detail_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            return Err(FiligreeClientError::HttpStatus {
                status: 503,
                body: "detail route down".to_owned(),
            });
        }
        Ok(self
            .details
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(issue_id)
            .cloned())
    }

    fn wardline_findings_for_path(
        &self,
        path: &str,
    ) -> Result<Vec<WardlineFinding>, FiligreeClientError> {
        self.wardline_path_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(path.to_owned());
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

    fn create_observation(
        &self,
        request: ObservationCreateRequest,
    ) -> Result<ObservationCreateResponse, FiligreeClientError> {
        let mut created = self
            .created_observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        created.push(request.clone());
        let observation_id = format!("loomweave-obs-{}", created.len());
        self.observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
                observation_id.clone(),
                ObservationRecord {
                    observation_id: observation_id.clone(),
                    summary: request.summary.clone(),
                    detail: request.detail.clone(),
                    file_path: request.file_path.clone().unwrap_or_default(),
                    line: request.line,
                    priority: request.priority,
                    actor: request.actor.clone(),
                },
            );
        Ok(ObservationCreateResponse { observation_id })
    }

    fn observation_by_id(
        &self,
        observation_id: &str,
    ) -> Result<Option<ObservationRecord>, FiligreeClientError> {
        Ok(self
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(observation_id)
            .cloned())
    }

    fn dismiss_observation(
        &self,
        observation_id: &str,
        _reason: &str,
    ) -> Result<(), FiligreeClientError> {
        self.dismissed_observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(observation_id.to_owned());
        Ok(())
    }
}

fn association(issue_id: &str, entity_id: &str, content_hash: &str) -> EntityAssociation {
    EntityAssociation {
        issue_id: issue_id.to_owned(),
        loomweave_entity_id: entity_id.to_owned(),
        content_hash_at_attach: content_hash.to_owned(),
        attached_at: "2026-05-17T00:00:00.000Z".to_owned(),
        attached_by: "codex".to_owned(),
    }
}

fn seed_alive_sei_binding(db_path: &std::path::Path, sei: &str, locator: &str) {
    let conn = Connection::open(db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO sei_bindings (
            sei, current_locator, body_hash, signature, status,
            born_run_id, updated_run_id, updated_at
         ) VALUES (
            ?1, ?2, 'hash-entry', NULL, 'alive', 'run-1', 'run-1',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        params![sei, locator],
    )
    .expect("insert alive SEI binding");
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
        .find(|tool| tool.name == "subsystem_member_list")
        .expect("subsystem_member_list tool definition");

    assert_eq!(
        tool.description,
        "List the module entities in a subsystem. Bounded: `limit` (default 50, max 100) + numeric-offset `cursor`."
    );
    assert_eq!(
        tool.input_schema,
        json!({
            "type": "object",
            "properties": {
                "id": {"type": "string", "minLength": 1},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                "cursor": {"type": ["string", "null"]}
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

    assert_eq!(envelope["ok"], true, "{envelope}");
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
async fn subsystem_members_blocked_member_keeps_navigable_identity() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    let subsystem_id = seed_subsystem(&conn, project.path());
    drop(conn);
    // The pkg.auth module's file carries a secret → its module entity is blocked.
    mark_blocked(&db_path, "python:module:pkg.auth", "secret_present");
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(&state, "subsystem_members", json!({"id": subsystem_id})).await;
    assert_eq!(envelope["ok"], true, "{envelope}");
    let members = envelope["result"]["members"].as_array().unwrap();
    // Member count stays honest; the blocked module keeps its navigable identity.
    assert_eq!(members.len(), 2, "{envelope}");
    let blocked = members
        .iter()
        .find(|m| m["briefing_blocked"] == "secret_present")
        .expect("blocked member present with identity");
    // Under A3 the blocked member's id/name/path ride alongside the flag.
    assert_eq!(blocked["id"], "python:module:pkg.auth", "{blocked}");
    assert!(!blocked["name"].is_null(), "{blocked}");
    assert!(!blocked["source_file_path"].is_null(), "{blocked}");
    // The visible member carries a null `briefing_blocked` flag.
    let visible = members
        .iter()
        .find(|m| m["id"] == "python:module:demo")
        .expect("visible member present");
    assert!(visible["briefing_blocked"].is_null(), "{visible}");
}

#[tokio::test]
async fn subsystem_members_re_withholds_secretlike_blocked_identity_fields() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    let subsystem_id = seed_subsystem(&conn, project.path());
    let secret = "fn_aGVsbG8gd29ybGQgc2VjcmV0IGtleSBhYmMxMjP8x9z";
    let secret_id = format!("python:module:{secret}");
    let secret_source_path = project.path().join("secret_module.py");
    std::fs::write(&secret_source_path, "def sentinel():\n    return True\n")
        .expect("write secret module source");
    insert_entity(
        &conn,
        &secret_id,
        "module",
        &secret_source_path,
        Some((1, 2)),
        None,
    );
    insert_edge(
        &conn,
        "in_subsystem",
        &secret_id,
        &subsystem_id,
        "resolved",
        None,
    );
    drop(conn);
    mark_blocked(&db_path, &secret_id, "secret_present");
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(&state, "subsystem_members", json!({"id": subsystem_id})).await;

    assert_eq!(envelope["ok"], true, "{envelope}");
    let members = envelope["result"]["members"].as_array().unwrap();
    let blocked = members
        .iter()
        .find(|m| m["briefing_blocked"] == "secret_present")
        .expect("blocked secretlike member present");
    assert!(
        blocked["id"].is_null(),
        "secretlike blocked id must be re-withheld: {blocked}"
    );
    assert!(
        blocked["name"].is_null(),
        "secretlike blocked name must be re-withheld: {blocked}"
    );
    assert!(
        !envelope.to_string().contains(secret),
        "secretlike identity leaked in subsystem members: {envelope}"
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
    let filigree_dir = project.path().join(".weft").join("filigree");
    fs::create_dir_all(&filigree_dir).unwrap();
    fs::write(filigree_dir.join("ephemeral.port"), "8542").unwrap();
    let config = FiligreeConfig {
        enabled: true,
        ..FiligreeConfig::default()
    };
    let diagnostics = DiagnosticsContext {
        llm: LlmDiagnostics {
            provider: "disabled".to_owned(),
            enabled: false,
            live: false,
            allow_live_provider: false,
            cache_max_age_days: 180,
        },
        filigree: resolve_filigree_url(&config, project.path(), |_| None),
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
async fn issues_for_queries_sei_before_locator_and_aliases_match_to_current_entity() {
    let (project, db_path) = open_project();
    seed_alive_sei_binding(
        &db_path,
        "loomweave:eid:demo-entry",
        "python:function:demo.entry",
    );
    let client = Arc::new(FakeFiligreeClient::default().with_response(
        "loomweave:eid:demo-entry",
        vec![association(
            "filigree-sei",
            "loomweave:eid:demo-entry",
            &expected_content_hash(project.path(), "python:function:demo.entry"),
        )],
    ));
    let state = state_for_filigree(project.path(), &db_path, client.clone());

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.entry", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["result"]["result_kind"], "matched", "{envelope}");
    let matched = &envelope["result"]["matched"][0];
    assert_eq!(matched["issue_id"], "filigree-sei");
    assert_eq!(matched["entity_id"], "python:function:demo.entry");
    assert_eq!(matched["association_entity_id"], "loomweave:eid:demo-entry");
    assert_eq!(matched["entity"]["sei"], "loomweave:eid:demo-entry");
    assert_eq!(
        client.calls(),
        vec!["loomweave:eid:demo-entry".to_owned()],
        "SEI should be the only lookup key when it is available"
    );
}

#[tokio::test]
async fn issues_for_falls_back_to_locator_when_sei_is_unavailable() {
    let (project, db_path) = open_project();
    let client = Arc::new(FakeFiligreeClient::default().with_response(
        "python:function:demo.entry",
        vec![association(
            "filigree-locator",
            "python:function:demo.entry",
            &expected_content_hash(project.path(), "python:function:demo.entry"),
        )],
    ));
    let state = state_for_filigree(project.path(), &db_path, client.clone());

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.entry", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(
        envelope["result"]["matched"][0]["issue_id"],
        "filigree-locator"
    );
    assert_eq!(
        envelope["result"]["matched"][0]["entity_id"],
        "python:function:demo.entry"
    );
    assert_eq!(
        client.calls(),
        vec!["python:function:demo.entry".to_owned()],
        "locator should be queried only when no SEI is available"
    );
}

#[tokio::test]
async fn issues_for_flags_drift_for_sei_bound_association() {
    let (project, db_path) = open_project();
    seed_alive_sei_binding(
        &db_path,
        "loomweave:eid:demo-entry",
        "python:function:demo.entry",
    );
    let current_hash = expected_content_hash(project.path(), "python:function:demo.entry");
    let client = Arc::new(FakeFiligreeClient::default().with_response(
        "loomweave:eid:demo-entry",
        vec![association(
            "filigree-drifted-sei",
            "loomweave:eid:demo-entry",
            "old-hash",
        )],
    ));
    let state = state_for_filigree(project.path(), &db_path, client.clone());

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.entry", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    let drifted = &envelope["result"]["drifted"][0];
    assert_eq!(drifted["issue_id"], "filigree-drifted-sei");
    assert_eq!(drifted["entity_id"], "python:function:demo.entry");
    assert_eq!(drifted["association_entity_id"], "loomweave:eid:demo-entry");
    assert_eq!(drifted["content_hash_at_attach"], "old-hash");
    assert_eq!(drifted["current_content_hash"], current_hash);
    assert_eq!(drifted["drift_status"], "drifted");
    assert_eq!(client.calls(), vec!["loomweave:eid:demo-entry".to_owned()]);
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
    let state =
        state_for(project.path(), &db_path).with_tool_policy(McpToolPolicy::allow_write_tools());

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
async fn summary_cache_key_and_prompt_include_matching_guidance() {
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
            summary_json: r#"{"purpose":"unguided"}"#.to_owned(),
            cost_usd: 0.001,
            tokens_input: 100,
            tokens_output: 20,
            caller_count: 0,
            fan_out: 2,
            stale_semantic: false,
            created_at: "2026-05-17T00:00:00.000Z".to_owned(),
            last_accessed_at: "2026-05-17T00:00:00.000Z".to_owned(),
        },
    )
    .unwrap();
    let guidance_properties = json!({
        "content": "Prefer operational risk notes when summarising functions.",
        "scope_level": "function",
        "match_rules": [{"type": "entity", "id": "python:function:demo.entry"}],
        "provenance": {"author": "test"},
        "authored_at": "2026-05-17T00:00:00.000Z"
    });
    upsert_guidance_sheet(
        &conn,
        &GuidanceSheetInput {
            id: "core:guidance:test-summary",
            name: "test-summary",
            short_name: "test-summary",
            properties: &guidance_properties,
        },
    )
    .unwrap();
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnySummaryProvider::new_output(
        r#"{"purpose":"guided"}"#,
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

    let cold = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(cold["ok"], true, "{cold}");
    assert_eq!(cold["result"]["cache"]["hit"], false);
    assert_eq!(cold["result"]["summary"]["purpose"], "guided");
    let invocation = provider
        .invocations()
        .into_iter()
        .next()
        .expect("summary provider invocation");
    assert!(
        invocation
            .prompt
            .contains("Prefer operational risk notes when summarising functions."),
        "summary prompt should include matching guidance: {}",
        invocation.prompt
    );

    let conn = Connection::open(&db_path).unwrap();
    let fingerprints: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT guidance_fingerprint FROM summary_cache \
                 WHERE entity_id = 'python:function:demo.entry' \
                 ORDER BY guidance_fingerprint",
            )
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert!(fingerprints.iter().any(|fp| fp == "guidance-empty"));
    assert!(
        fingerprints
            .iter()
            .any(|fp| fp.starts_with("guidance:") && fp != "guidance-empty"),
        "guided summary should use a non-empty guidance fingerprint: {fingerprints:?}"
    );
    drop(conn);

    let warm = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(warm["ok"], true, "{warm}");
    assert_eq!(warm["result"]["cache"]["hit"], true);
    assert_eq!(warm["result"]["summary"]["purpose"], "guided");
    assert_eq!(provider.invocations().len(), 1);

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn summary_keeps_future_guidance_under_unix_clock() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    let guidance_properties = json!({
        "content": "Future-dated guidance must still reach summary prompts.",
        "scope_level": "function",
        "expires": "2999-12-31T00:00:00.000Z",
        "match_rules": [{"type": "entity", "id": "python:function:demo.entry"}],
        "provenance": {"author": "test"},
        "authored_at": "2026-05-17T00:00:00.000Z"
    });
    upsert_guidance_sheet(
        &conn,
        &GuidanceSheetInput {
            id: "core:guidance:test-summary-future",
            name: "test-summary-future",
            short_name: "test-summary-future",
            properties: &guidance_properties,
        },
    )
    .unwrap();
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnySummaryProvider::new_output(
        r#"{"purpose":"guided"}"#,
        120,
        0.0,
    ));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    )
    .with_clock(|| "unix:1748822400".to_owned());

    let cold = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(cold["ok"], true, "{cold}");
    let invocation = provider
        .invocations()
        .into_iter()
        .next()
        .expect("summary provider invocation");
    assert!(
        invocation
            .prompt
            .contains("Future-dated guidance must still reach summary prompts."),
        "summary prompt should include future guidance under unix clock: {}",
        invocation.prompt
    );

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn summary_preview_cost_counts_future_guidance_under_unix_clock() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).unwrap();
    let guidance_properties = json!({
        "content": "Future-dated guidance must still reach preview estimates.",
        "scope_level": "function",
        "expires": "2999-12-31T00:00:00.000Z",
        "match_rules": [{"type": "entity", "id": "python:function:demo.entry"}],
        "provenance": {"author": "test"},
        "authored_at": "2026-05-17T00:00:00.000Z"
    });
    upsert_guidance_sheet(
        &conn,
        &GuidanceSheetInput {
            id: "core:guidance:test-summary-preview-future",
            name: "test-summary-preview-future",
            short_name: "test-summary-preview-future",
            properties: &guidance_properties,
        },
    )
    .unwrap();
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnySummaryProvider::new_output(
        r#"{"purpose":"unused"}"#,
        120,
        0.0,
    ));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    )
    .with_clock(|| "unix:1748822400".to_owned());

    let envelope = call_tool(
        &state,
        "summary_preview_cost",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    let prompt = build_leaf_summary_prompt(&LeafSummaryPromptInput {
        entity_id: "python:function:demo.entry".to_owned(),
        kind: "function".to_owned(),
        name: "python:function:demo.entry".to_owned(),
        guidance: "Guidance sheet core:guidance:test-summary-preview-future:\n\
                   Future-dated guidance must still reach preview estimates."
            .to_owned(),
        source_excerpt: expected_source_excerpt(project.path(), "python:function:demo.entry"),
    });
    let expected_tokens = i64::try_from(prompt.body.chars().count())
        .unwrap_or(i64::MAX)
        .saturating_add(3)
        / 4;

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(envelope["result"]["cache_status"], "miss");
    assert_eq!(
        envelope["result"]["estimated_input_tokens"], expected_tokens,
        "preview estimate should include future guidance under unix clock: {envelope}"
    );
    assert!(
        provider.invocations().is_empty(),
        "preview must not call the LLM provider"
    );

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn propose_guidance_creates_observation_and_promote_makes_sheet_visible() {
    let (project, db_path) = open_project();
    let client = Arc::new(FakeFiligreeClient::default());
    let state = state_for_filigree(project.path(), &db_path, client.clone())
        .with_clock(|| "unix:1748822400".to_owned());

    let proposed = call_tool(
        &state,
        "propose_guidance",
        json!({
            "entity_id": "python:function:demo.entry",
            "content": "Prefer operational risk notes when summarising entrypoints.",
            "scope_level": "function",
            "name": "demo-entry-risk",
            "pinned": true
        }),
    )
    .await;

    assert_eq!(proposed["ok"], true);
    assert_eq!(proposed["result"]["observation_id"], "loomweave-obs-1");
    let created = client.created_observations();
    assert_eq!(created.len(), 1);
    assert!(created[0].summary.contains("python:function:demo.entry"));

    let inert = call_tool(
        &state,
        "guidance_for",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(inert["ok"], true);
    assert_eq!(
        inert["result"]["guidance"]
            .as_array()
            .expect("guidance array")
            .len(),
        0,
        "a proposal must not be composed before promotion"
    );

    let promoted = call_tool(
        &state,
        "promote_guidance",
        json!({"observation_id": "loomweave-obs-1"}),
    )
    .await;
    assert_eq!(promoted["ok"], true);
    assert_eq!(
        promoted["result"]["sheet_id"],
        "core:guidance:demo-entry-risk"
    );
    assert_eq!(
        client.dismissed_observations(),
        vec!["loomweave-obs-1".to_owned()]
    );

    let visible = call_tool(
        &state,
        "guidance_for",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(visible["ok"], true);
    let sheets = visible["result"]["guidance"]
        .as_array()
        .expect("guidance array");
    assert_eq!(sheets.len(), 1);
    assert_eq!(sheets[0]["id"], "core:guidance:demo-entry-risk");
    assert_eq!(
        sheets[0]["content"],
        "Prefer operational risk notes when summarising entrypoints."
    );
    assert_eq!(sheets[0]["provenance"], "filigree_promotion");
    assert_eq!(sheets[0]["matched_by"], json!(["entity"]));

    let conn = Connection::open(&db_path).unwrap();
    let authored_at: String = conn
        .query_row(
            "SELECT json_extract(properties, '$.authored_at') \
             FROM entities WHERE id = 'core:guidance:demo-entry-risk'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(authored_at, "2025-06-02T00:00:00.000Z");
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
        assert!(
            request.contains(r#""response_format":{"json_schema":{"name":"loomweave_summary""#)
        );
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
            referer: "https://github.com/foundryside-dev/loomweave".to_owned(),
            title: "Loomweave Test".to_owned(),
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

#[tokio::test]
async fn status_surfaces_agree_on_allow_live_provider_when_half_configured() {
    // agent-first-feedback §2.2: project_status_get and summary_preview_cost must
    // report the SAME allow_live_provider for a half-configured state — a provider
    // permitted by config (allow_live_provider: true) but with enabled=false, so
    // no live provider is wired. Previously the two read paths disagreed (status
    // read raw config → true; preview read the unwired provider → false).
    let (project, db_path) = open_project();
    let diagnostics = DiagnosticsContext {
        llm: LlmDiagnostics {
            provider: "disabled".to_owned(),
            enabled: false,
            live: false,
            allow_live_provider: true, // configured-but-inert
            cache_max_age_days: 180,
        },
        filigree: resolve_filigree_url(&FiligreeConfig::default(), project.path(), |_| None),
    };
    let state = state_for(project.path(), &db_path).with_diagnostics(diagnostics);

    let status = call_tool(&state, "project_status", json!({})).await;
    let preview = call_tool(
        &state,
        "summary_preview_cost",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;

    assert_eq!(status["result"]["llm"]["allow_live_provider"], true);
    assert_eq!(
        status["result"]["llm"]["allow_live_provider"],
        preview["result"]["policy"]["allow_live_provider"],
        "status surfaces disagree on allow_live_provider: status={status:?} preview={preview:?}"
    );
    // Both must also agree the live path is off, so a miss would not spend.
    assert_eq!(status["result"]["llm"]["enabled"], false);
    assert_eq!(preview["result"]["policy"]["enabled"], false);
    assert_eq!(
        status["result"]["llm"]["live"],
        preview["result"]["policy"]["live"]
    );
    assert_eq!(preview["result"]["live_spend_would_occur"], false);
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
        "LMWV-LLM-TOKEN-CEILING-EXCEEDED"
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
    assert_eq!(suggested[0]["tool"], "entity_source_get");

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
async fn source_for_entity_reports_context_drift_before_returning_context() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    // Keep demo.mid's indexed span (lines 4-5) unchanged, but mutate a
    // requested context line. The entity span hash still matches, so this
    // specifically exercises the source-file hash guard that covers context.
    std::fs::write(
        project.path().join("demo.py"),
        "def entry():
    return mid()
API_TOKEN = 'SECRET_CONTEXT_DRIFT'
def mid():
    return target()

def target():
    return 1
",
    )
    .expect("rewrite source context");

    let resp = call_tool(
        &state,
        "source_for_entity",
        json!({"id": "python:function:demo.mid", "context_lines": 1}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    assert_eq!(resp["result"]["source_status"], "drifted");
    assert!(resp["result"].get("lines").is_none());
    assert!(
        !resp.to_string().contains("SECRET_CONTEXT_DRIFT"),
        "leaked drifted context: {resp}"
    );
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

    let callee_resp = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:demo.mid", "role": "callee", "kind": "calls"}),
    )
    .await;
    assert_eq!(callee_resp["ok"], true, "{callee_resp:?}");
    let callee_sites = callee_resp["result"]["sites"]
        .as_array()
        .expect("callee sites array");
    assert!(!callee_sites.is_empty(), "{callee_resp:?}");
    for site in callee_sites {
        assert_eq!(site["line_text"], "", "{site:?}");
        assert_eq!(site["briefing_blocked"], true, "{site:?}");
        assert_eq!(site["source_status"], "briefing_blocked", "{site:?}");
    }
}

#[tokio::test]
async fn call_sites_redacts_line_text_for_drifted_owner() {
    // call_sites reads the site owner's source file to populate line_text. If
    // that file no longer matches the indexed content_hash, it must not return
    // newly modified, unscanned source content through either caller or callee
    // queries.
    let (project, db_path) = open_project();
    let secret_line = "def entry():  # DRIFT_ONLY_SECRET=sk-validation-12345\n";
    std::fs::write(
        project.path().join("demo.py"),
        format!(
            "{secret_line}    return mid()\n\ndef mid():\n    return target()\n\ndef target():\n    return 1\n"
        ),
    )
    .expect("rewrite source after indexing");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:demo.mid", "role": "callee", "kind": "calls"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    let sites = resp["result"]["sites"].as_array().expect("sites array");
    assert!(!sites.is_empty(), "{resp:?}");
    for site in sites {
        assert_eq!(
            site["line_text"], "",
            "drifted owner leaked source: {site:?}"
        );
        assert_eq!(site["source_status"], "drifted", "{site:?}");
        assert!(site["drift"]["stored_content_hash"].is_string(), "{site:?}");
        assert!(
            site["drift"]["current_content_hash"].is_string(),
            "{site:?}"
        );
    }
    let blob = resp.to_string();
    assert!(
        !blob.contains("DRIFT_ONLY_SECRET"),
        "leaked drifted source: {blob}"
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

// ---- briefing_blocked identity gate (clarion-307668e2be) ----------------
//
// A secret-scan-blocked entity's identity (id, name, source_file_path, line
// span) must not be disclosed by any discovery/structure MCP read — matching
// the federation read API (ADR-034). Discovery surfaces emit a *stub* that
// acknowledges existence with only the block reason; structure-fan-out surfaces
// (neighborhood / orientation) *refuse* when the queried entity itself is
// blocked. The blocked entity's qualname-bearing id must appear NOWHERE in the
// response (it leaks the name even when name/path are nulled).

/// Mark an already-seeded entity briefing-blocked by rewriting its `properties`.
fn mark_blocked(db_path: &std::path::Path, id: &str, reason: &str) {
    let conn = Connection::open(db_path).expect("open sqlite");
    conn.execute(
        "UPDATE entities SET properties = ?1 WHERE id = ?2",
        params![json!({"briefing_blocked": reason}).to_string(), id],
    )
    .expect("mark entity blocked");
}

/// Assert a briefing-blocked entity projection keeps its navigable identity
/// (clarion-719e7320f5, A3): `id`, `kind`, `name`, `short_name`,
/// `source_file_path`, the line span and `content_hash` are PRESENT alongside
/// the `briefing_blocked` flag; only the cross-tool `sei` binding key stays null.
/// The secret is the file content, not the entity's structural identity.
fn assert_blocked_identity_present(entity: &Value, reason: &str) {
    assert_eq!(entity["briefing_blocked"], reason, "block reason: {entity}");
    for field in [
        "id",
        "kind",
        "name",
        "short_name",
        "source_file_path",
        "source_line_start",
        "source_line_end",
        "content_hash",
    ] {
        assert!(
            entity.get(field).is_some_and(|v| !v.is_null()),
            "identity field `{field}` must be PRESENT for a blocked entity: {entity}"
        );
    }
    assert!(
        entity["sei"].is_null(),
        "SEI must stay null for a blocked entity (ADR-034): {entity}"
    );
}

#[tokio::test]
async fn find_entity_redacts_briefing_blocked_identity() {
    let (project, db_path) = open_project();
    mark_blocked(&db_path, "python:function:demo.mid", "secret_present");
    let state = state_for(project.path(), &db_path);

    // `demo.mid` matches exactly one seeded id.
    let resp = call_tool(
        &state,
        "find_entity",
        json!({"pattern": "python:function:demo.mid", "limit": 10}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");

    let entities = resp["result"]["entities"].as_array().unwrap();
    let blocked: Vec<&Value> = entities
        .iter()
        .filter(|e| e["briefing_blocked"] == "secret_present")
        .collect();
    assert_eq!(blocked.len(), 1, "blocked entity still listed: {resp}");
    assert_blocked_identity_present(blocked[0], "secret_present");
    // The navigable locator IS exposed now (A3): identity is not the secret.
    assert_eq!(blocked[0]["id"], "python:function:demo.mid", "{resp}");
}

#[tokio::test]
async fn entity_at_redacts_briefing_blocked_match_and_context() {
    let (project, db_path) = open_project();
    // demo.mid uniquely spans lines 4-5 (module also spans, but mid is innermost).
    mark_blocked(&db_path, "python:function:demo.mid", "secret_present");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(&state, "entity_at", json!({"file": "demo.py", "line": 4})).await;
    assert_eq!(resp["ok"], true, "{resp}");
    assert_blocked_identity_present(&resp["result"]["entity"], "secret_present");

    // The matched node in the containing stack keeps its navigable identity (A3),
    // and the ranges block keeps the line span — only the secret content hides.
    let stack = resp["result"]["entity_context"]["containing_stack"]
        .as_array()
        .unwrap();
    let matched_node = stack.last().expect("matched node present");
    assert_eq!(matched_node["briefing_blocked"], "secret_present", "{resp}");
    assert_eq!(matched_node["id"], "python:function:demo.mid", "{resp}");
    assert!(
        matched_node["sei"].is_null(),
        "stack SEI must be null: {resp}"
    );
    assert!(
        !resp["result"]["entity_context"]["ranges"]["source_line_start"].is_null(),
        "matched ranges must expose the line span now (A3): {resp}"
    );
}

#[tokio::test]
async fn entity_at_redacts_blocked_alternative() {
    let (project, db_path) = open_project();
    // demo.target and demo.alt_target both span lines 7-8 (same-granularity
    // overlap). entity_at(line 7) matches alt_target (id-sorted first) and lists
    // target as a same-granularity *alternative* — which must be redacted when
    // blocked (the `alternatives` block is a third projection in entity_context).
    mark_blocked(&db_path, "python:function:demo.target", "secret_present");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(&state, "entity_at", json!({"file": "demo.py", "line": 7})).await;
    assert_eq!(resp["ok"], true, "{resp}");
    let alternatives = resp["result"]["entity_context"]["alternatives"]
        .as_array()
        .unwrap();
    let blocked = alternatives
        .iter()
        .find(|a| a["entity"]["briefing_blocked"] == "secret_present")
        .expect("blocked alternative present as a stub");
    assert_blocked_identity_present(&blocked["entity"], "secret_present");
    assert_eq!(
        blocked["entity"]["id"], "python:function:demo.target",
        "{resp}"
    );
}

#[tokio::test]
async fn orientation_suggested_reads_offer_blocked_callee_drilldown() {
    let (project, db_path) = open_project();
    // entry's first resolved callee is mid; block it. Under A3
    // (clarion-719e7320f5) the blocked callee keeps its navigable id, so the
    // suggested-reads drill-down into the first callee IS offered — the entity is
    // reachable by locator even though its source content stays withheld.
    mark_blocked(&db_path, "python:function:demo.mid", "secret_present");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    assert!(
        resp["result"]["suggested_next_reads"]
            .to_string()
            .contains("python:function:demo.mid"),
        "blocked callee drill-down should be offered now (A3): {resp}"
    );
}

#[tokio::test]
async fn neighborhood_blocked_neighbor_keeps_navigable_identity() {
    let (project, db_path) = open_project();
    // demo.entry calls demo.mid (resolved). Block the callee.
    mark_blocked(&db_path, "python:function:demo.mid", "secret_present");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");

    let callees = resp["result"]["callees"].as_array().unwrap();
    let blocked = callees
        .iter()
        .find(|c| c["entity"]["briefing_blocked"] == "secret_present")
        .expect("blocked callee present as a stub");
    // Under A3 the blocked callee keeps its navigable identity; `stored_to_id`
    // echoes the same (now non-secret) id.
    assert_blocked_identity_present(&blocked["entity"], "secret_present");
    assert_eq!(
        blocked["entity"]["id"], "python:function:demo.mid",
        "{resp}"
    );
    assert_eq!(
        blocked["stored_to_id"], "python:function:demo.mid",
        "stored_to_id echoes the navigable callee id: {blocked}"
    );
}

#[tokio::test]
async fn neighborhood_refuses_structure_for_blocked_entity() {
    let (project, db_path) = open_project();
    mark_blocked(&db_path, "python:function:demo.mid", "secret_present");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:demo.mid"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    assert_eq!(resp["result"]["available"], false, "{resp}");
    assert_eq!(
        resp["result"]["briefing_blocked"], "secret_present",
        "{resp}"
    );
    // No structure around the withheld entity, and its id never appears.
    assert!(resp["result"]["callers"].is_null(), "{resp}");
    assert!(resp["result"]["callees"].is_null(), "{resp}");
    assert!(
        !resp.to_string().contains("demo.mid"),
        "blocked id leaked in neighborhood refusal: {resp}"
    );
}

#[tokio::test]
async fn orientation_refuses_for_blocked_primary() {
    let (project, db_path) = open_project();
    mark_blocked(&db_path, "python:function:demo.entry", "secret_present");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    assert_eq!(resp["result"]["available"], false, "{resp}");
    assert_eq!(
        resp["result"]["briefing_blocked"], "secret_present",
        "{resp}"
    );
    assert!(resp["result"]["primary_entity"].is_null(), "{resp}");
    assert!(
        !resp.to_string().contains("demo.entry"),
        "blocked primary id leaked in orientation refusal: {resp}"
    );
}

#[tokio::test]
async fn orientation_keeps_blocked_node_navigable_in_execution_paths() {
    let (project, db_path) = open_project();
    // demo.target is a leaf reached via entry -> mid -> target (resolved).
    mark_blocked(&db_path, "python:function:demo.target", "secret_present");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");

    // Under A3 the blocked node keeps its navigable identity in the node table,
    // flagged briefing_blocked, with a null SEI.
    let nodes = resp["result"]["execution_paths"]["nodes"]
        .as_array()
        .unwrap();
    let blocked_node = nodes
        .iter()
        .find(|n| n["id"] == "python:function:demo.target")
        .expect("blocked node present with identity");
    assert_eq!(blocked_node["briefing_blocked"], "secret_present", "{resp}");
    assert!(
        blocked_node["sei"].is_null(),
        "node SEI must be null: {resp}"
    );
    // …and its id rides through the path arrays unchanged (no sentinel).
    let paths = resp["result"]["execution_paths"]["paths"]
        .as_array()
        .unwrap();
    let has_blocked_in_path = paths.iter().any(|p| {
        p.as_array()
            .unwrap()
            .iter()
            .any(|id| id == "python:function:demo.target")
    });
    assert!(
        has_blocked_in_path,
        "blocked id should ride the path: {resp}"
    );
    let has_sentinel = paths.iter().any(|p| {
        p.as_array()
            .unwrap()
            .iter()
            .any(|id| id == "[briefing-blocked]")
    });
    assert!(
        !has_sentinel,
        "no sentinel for a navigable blocked node: {resp}"
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

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["callers"][0]["entity"]["id"],
        "python:function:demo.entry"
    );
    assert_eq!(
        envelope["result"]["callers"][0]["edge_confidence"],
        "inferred"
    );
    let next_action = envelope["result"]["next_action"]
        .as_str()
        .expect("next_action string");
    assert!(
        !next_action.contains("NOT in `callers`"),
        "inferred dispatch materialized the caller, so next_action must not claim unresolved sites are absent from callers: {envelope}"
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
async fn callers_of_inferred_ignores_stale_unresolved_call_sites() {
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
    insert_unresolved_call_site(&conn, "python:function:demo.entry", "site-stale", "dynamic");
    conn.execute(
        "UPDATE entities SET content_hash = 'hash-after-body-change' \
         WHERE id = 'python:function:demo.entry'",
        [],
    )
    .expect("simulate a changed caller body without authoritative unresolved rows");
    drop(conn);

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnyInferredProvider::new(
        r#"{"edges":[{"site_key":"site-stale","target_id":"python:function:demo.dynamic","confidence":0.91,"rationale":"stale"}]}"#,
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

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(envelope["result"]["callers"].as_array().unwrap().len(), 0);
    assert!(
        provider.invocations().is_empty(),
        "stale unresolved rows must not trigger inferred dispatch"
    );

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
    // The live `ctx.dynamic` site both suffix-matches the target name and
    // makes the project-wide unresolved blind spot real (clarion-df87b4f381).
    assert_eq!(
        resolved["result"]["scope_excludes"],
        json!(["attribute-receiver-calls", "unresolved-static-calls"])
    );
    assert_eq!(resolved["result"]["unresolved_name_matches"], 1);

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
    let provider = Arc::new(GatedInferredProvider::new(
        r#"{"edges":[{"site_key":"site-dynamic","target_id":"python:function:demo.dynamic","confidence":0.91,"rationale":"name match"}]}"#,
    ));
    let state = Arc::new(state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    ));
    let args = json!({"id": "python:function:demo.dynamic", "confidence": "inferred"});

    let first_state = Arc::clone(&state);
    let first_args = args.clone();
    let first_handle =
        tokio::spawn(async move { call_tool(&first_state, "callers_of", first_args).await });
    provider.wait_started().await;

    let (first, second) = {
        let second_future = call_tool(&state, "callers_of", args);
        tokio::pin!(second_future);
        tokio::select! {
            completed = &mut second_future => panic!("follower completed before leader released: {completed}"),
            () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
        }
        provider.release();

        let (first, second) = tokio::join!(first_handle, second_future);
        (first.expect("leader callers_of task"), second)
    };

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
async fn callers_of_with_no_skipped_candidate_reports_traversal_complete() {
    // Per-query honesty (clarion-76c31b730a): the blanket scope_excludes footer
    // is gone. A target with NO name-matched unresolved call sites had nothing
    // skipped, so scope_excludes is empty and traversal_complete confirms every
    // candidate was searched — an empty callers list is now a true negative.
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
        json!([]),
        "{envelope}"
    );
    assert_eq!(
        envelope["result"]["traversal_complete"], true,
        "nothing skipped -> traversal_complete: {envelope}"
    );
    assert_eq!(
        envelope["result"]["unresolved_candidates"],
        json!([]),
        "{envelope}"
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
    // Per-query honesty (clarion-76c31b730a): the module has no name-matched
    // unresolved call sites, so nothing was skipped — no blanket footer.
    assert!(
        excludes.is_empty(),
        "no skipped candidate -> empty scope_excludes, got {excludes:?}"
    );
    assert_eq!(envelope["result"]["traversal_complete"], true, "{envelope}");
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
    // Per-query honesty (clarion-76c31b730a): nothing skipped for this target.
    assert_eq!(
        envelope["result"]["scope_excludes"],
        json!([]),
        "{envelope}"
    );
    assert_eq!(envelope["result"]["traversal_complete"], true, "{envelope}");
}

// ── unresolved name-matched call-site honesty (clarion-df87b4f381) ────────────
//
// The store records statically-unbindable call sites in
// `entity_unresolved_call_sites`; when any LIVE rows name-match the queried
// target, an empty/short `callers` list is not a true negative. The navigation
// surface must say so: an `unresolved_name_matches` count, the
// `unresolved-static-calls` scope_excludes marker, and a `next_action` pointer
// at the evidence tool (`entity_call_site_list role=callee`), which also works
// when an operator explicitly disables write tools and `confidence=inferred` is
// policy-gated.

#[tokio::test]
async fn callers_of_counts_unresolved_name_matches_and_names_the_blind_spot() {
    let (project, db_path) = open_project();
    {
        let conn = Connection::open(&db_path).unwrap();
        // A bare-name unresolved site whose callee_expr equals the target's
        // short name — the dominant unresolved cross-module call shape.
        insert_unresolved_call_site(&conn, "python:function:demo.entry", "site-bare", "target");
    }
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.target"}),
    )
    .await;

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["unresolved_name_matches"], 1,
        "{envelope}"
    );
    let excludes = envelope["result"]["scope_excludes"]
        .as_array()
        .expect("scope_excludes array");
    assert!(
        excludes.iter().any(|v| v == "unresolved-static-calls"),
        "live unresolved sites must be declared as a blind spot: {envelope}"
    );
    assert!(
        excludes.iter().any(|v| v == "attribute-receiver-calls"),
        "{envelope}"
    );
    let next_action = envelope["result"]["next_action"]
        .as_str()
        .expect("next_action string when matches exist");
    assert!(
        next_action.contains("entity_call_site_list"),
        "next_action must point at the evidence tool: {envelope}"
    );
    assert!(next_action.contains("callee"), "{envelope}");
    // Per-query honesty (clarion-76c31b730a): a skipped candidate means the
    // traversal is incomplete, and the skipped site is surfaced as an
    // unresolved_candidate (the in-tool grep-fallback).
    assert_eq!(
        envelope["result"]["traversal_complete"], false,
        "a skipped candidate -> traversal_complete false: {envelope}"
    );
    let candidates = envelope["result"]["unresolved_candidates"]
        .as_array()
        .expect("unresolved_candidates array");
    assert_eq!(candidates.len(), 1, "{envelope}");
    assert_eq!(candidates[0]["callee_text"], "target", "{envelope}");
    // A bare-name callee (no dot) is a dynamic dispatch, not an attribute
    // receiver.
    assert_eq!(candidates[0]["why"], "dynamic", "{envelope}");

    // The shared vocabulary also covers calls-only path traversal.
    let paths = call_tool(
        &state,
        "execution_paths_from",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(paths["ok"], true, "{paths}");
    assert!(
        paths["result"]["scope_excludes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "unresolved-static-calls"),
        "{paths}"
    );

    // call_sites SEARCHES the unresolved table (it returns unresolved_sites),
    // so it must NOT declare the category as an unsearched blind spot.
    let sites = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:demo.target", "role": "callee"}),
    )
    .await;
    assert_eq!(sites["ok"], true, "{sites}");
    assert!(
        !sites["result"]["scope_excludes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "unresolved-static-calls"),
        "call_sites surfaces unresolved sites; the marker would be false: {sites}"
    );
    assert_eq!(
        sites["result"]["unresolved_sites"][0]["callee_expr"], "target",
        "{sites}"
    );
}

#[tokio::test]
async fn callers_of_without_unresolved_sites_reports_zero_matches_and_no_marker() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.target"}),
    )
    .await;

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["unresolved_name_matches"], 0,
        "{envelope}"
    );
    assert!(
        envelope["result"]["next_action"].is_null(),
        "no matches -> no recovery pointer: {envelope}"
    );
    // Per-query honesty (clarion-76c31b730a): no name-matched candidate was
    // skipped, so scope_excludes is empty and traversal_complete is true — the
    // blanket attribute-receiver footer no longer fires on a clean target.
    assert_eq!(
        envelope["result"]["scope_excludes"],
        json!([]),
        "{envelope}"
    );
    assert_eq!(envelope["result"]["traversal_complete"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["unresolved_candidates"],
        json!([]),
        "{envelope}"
    );
}

#[tokio::test]
async fn callers_of_ignores_stale_unresolved_rows_in_count_and_marker() {
    let (project, db_path) = open_project();
    {
        let conn = Connection::open(&db_path).unwrap();
        insert_unresolved_call_site(&conn, "python:function:demo.entry", "site-stale", "target");
        conn.execute(
            "UPDATE entities SET content_hash = 'hash-after-body-change' \
             WHERE id = 'python:function:demo.entry'",
            [],
        )
        .expect("simulate a changed caller body");
    }
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.target"}),
    )
    .await;

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["unresolved_name_matches"], 0,
        "stale rows (content-hash mismatch) are not evidence: {envelope}"
    );
    // Per-query honesty (clarion-76c31b730a): a stale row matched nothing live,
    // so nothing was skipped — empty scope_excludes, traversal_complete true,
    // and no unresolved_candidates surfaced.
    assert_eq!(
        envelope["result"]["scope_excludes"],
        json!([]),
        "{envelope}"
    );
    assert_eq!(envelope["result"]["traversal_complete"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["unresolved_candidates"],
        json!([]),
        "{envelope}"
    );
}

// ── per-query caller honesty (clarion-76c31b730a) ────────────────────────────
//
// scope_excludes is now populated ONLY when this traversal actually skipped a
// name-matched candidate. When nothing was skipped, scope_excludes is empty
// AND traversal_complete:true confirms every candidate was searched. The
// skipped sites are surfaced as unresolved_candidates [{path, line,
// callee_text, why}] — the in-tool grep-fallback.

#[tokio::test]
async fn callers_of_unresolved_candidates_classify_attribute_receiver_vs_dynamic() {
    let (project, db_path) = open_project();
    {
        let conn = Connection::open(&db_path).unwrap();
        // A dotted callee text is an attribute/method receiver the static
        // resolver cannot bind; a bare name is a dynamic dispatch.
        insert_unresolved_call_site(
            &conn,
            "python:function:demo.entry",
            "site-attr",
            "obj.target",
        );
        insert_unresolved_call_site(&conn, "python:function:demo.mid", "site-dyn", "target");
    }
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.target"}),
    )
    .await;

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["traversal_complete"], false,
        "{envelope}"
    );
    let candidates = envelope["result"]["unresolved_candidates"]
        .as_array()
        .expect("unresolved_candidates array");
    assert_eq!(candidates.len(), 2, "{envelope}");
    let why_for = |callee: &str| {
        candidates
            .iter()
            .find(|c| c["callee_text"] == callee)
            .and_then(|c| c["why"].as_str())
            .unwrap_or_else(|| panic!("no candidate for {callee}: {envelope}"))
    };
    assert_eq!(why_for("obj.target"), "attribute-receiver", "{envelope}");
    assert_eq!(why_for("target"), "dynamic", "{envelope}");
    // Each candidate carries the call-site location (in-tool grep-fallback).
    for c in candidates {
        assert!(c["path"].is_string(), "candidate needs a path: {envelope}");
        assert!(
            c.get("line").is_some(),
            "candidate needs a line field: {envelope}"
        );
    }
}

#[tokio::test]
async fn callers_of_unresolved_candidates_redacts_callee_text_for_blocked_owner() {
    let (project, db_path) = open_project();
    let secret = "fn_aGVsbG8gd29ybGQgc2VjcmV0IGtleSBhYmMxMjP8x9z";
    {
        let conn = Connection::open(&db_path).unwrap();
        insert_unresolved_call_site(
            &conn,
            "python:function:demo.entry",
            "site-secret-receiver",
            &format!("{secret}.target"),
        );
    }
    mark_blocked(&db_path, "python:function:demo.entry", "secret_present");
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.target"}),
    )
    .await;

    assert_eq!(envelope["ok"], true, "{envelope}");
    let candidates = envelope["result"]["unresolved_candidates"]
        .as_array()
        .expect("unresolved_candidates array");
    assert_eq!(candidates.len(), 1, "{envelope}");
    assert!(
        candidates[0]["callee_text"].is_null(),
        "blocked owner must not echo parsed source callee text: {envelope}"
    );
    assert_eq!(candidates[0]["why"], "attribute-receiver", "{envelope}");
    assert!(
        !envelope.to_string().contains(secret),
        "blocked owner callee expression leaked: {envelope}"
    );
}

#[tokio::test]
async fn neighborhood_with_no_skipped_candidate_reports_traversal_complete() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:demo.target"}),
    )
    .await;

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["scope_excludes"],
        json!([]),
        "{envelope}"
    );
    assert_eq!(envelope["result"]["traversal_complete"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["unresolved_candidates"],
        json!([]),
        "{envelope}"
    );
}

#[tokio::test]
async fn neighborhood_surfaces_unresolved_candidates_when_a_candidate_is_skipped() {
    let (project, db_path) = open_project();
    {
        let conn = Connection::open(&db_path).unwrap();
        insert_unresolved_call_site(&conn, "python:function:demo.entry", "site-bare", "target");
    }
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:demo.target"}),
    )
    .await;

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["traversal_complete"], false,
        "{envelope}"
    );
    let candidates = envelope["result"]["unresolved_candidates"]
        .as_array()
        .expect("unresolved_candidates array");
    assert_eq!(candidates.len(), 1, "{envelope}");
    assert_eq!(candidates[0]["callee_text"], "target", "{envelope}");
    assert_eq!(candidates[0]["why"], "dynamic", "{envelope}");
}

// A1 (review): `confidence=inferred` forces `scope_excludes` empty (the inferred
// dispatch attempts the unresolved category, so it skips nothing) which sets
// `traversal_complete: true`. `unresolved_candidates` MUST therefore also be
// empty on the inferred path — surfacing name-matched-but-skipped sites there
// would contradict the documented completeness contract ("empty scope_excludes
// with traversal_complete:true means every candidate was searched"). These
// tests exercise the write-tools-enabled posture, the only posture where
// `confidence=inferred` is permitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn callers_of_inferred_does_not_contradict_traversal_complete() {
    let (project, db_path) = open_project();
    {
        let conn = Connection::open(&db_path).unwrap();
        // A live unresolved call site that name-matches `target` — for the
        // default `resolved` confidence this is a skipped candidate
        // (traversal_complete:false + a non-empty unresolved_candidates row).
        insert_unresolved_call_site(&conn, "python:function:demo.entry", "site-bare", "target");
    }

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnyInferredProvider::new(r#"{"edges":[]}"#));
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

    assert_eq!(envelope["ok"], true, "{envelope}");
    // Inferred dispatch attempts the unresolved category: scope_excludes is
    // empty and the traversal is reported complete.
    assert_eq!(
        envelope["result"]["scope_excludes"],
        json!([]),
        "{envelope}"
    );
    assert_eq!(envelope["result"]["traversal_complete"], true, "{envelope}");
    // ...so unresolved_candidates MUST be empty too — no contradiction.
    assert_eq!(
        envelope["result"]["unresolved_candidates"],
        json!([]),
        "inferred path must not surface candidates alongside traversal_complete:true: {envelope}"
    );

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn neighborhood_inferred_does_not_contradict_traversal_complete() {
    let (project, db_path) = open_project();
    {
        let conn = Connection::open(&db_path).unwrap();
        insert_unresolved_call_site(&conn, "python:function:demo.entry", "site-bare", "target");
    }

    let (writer, handle) = Writer::spawn(db_path.clone(), 50, 256).unwrap();
    let provider = Arc::new(AnyInferredProvider::new(r#"{"edges":[]}"#));
    let state = state_for_summary(
        project.path(),
        &db_path,
        &writer,
        provider.clone(),
        llm_config(),
    );

    let envelope = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:demo.target", "confidence": "inferred"}),
    )
    .await;

    assert_eq!(envelope["ok"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["scope_excludes"],
        json!([]),
        "{envelope}"
    );
    assert_eq!(envelope["result"]["traversal_complete"], true, "{envelope}");
    assert_eq!(
        envelope["result"]["unresolved_candidates"],
        json!([]),
        "inferred path must not surface candidates alongside traversal_complete:true: {envelope}"
    );

    drop(state);
    drop(writer);
    handle.await.unwrap().unwrap();
}

#[tokio::test]
async fn neighborhood_and_orientation_carry_unresolved_name_matches() {
    let (project, db_path) = open_project();
    {
        let conn = Connection::open(&db_path).unwrap();
        insert_unresolved_call_site(&conn, "python:function:demo.entry", "site-bare", "target");
    }
    let state = state_for(project.path(), &db_path);

    let neighborhood = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:demo.target"}),
    )
    .await;
    assert_eq!(neighborhood["ok"], true, "{neighborhood}");
    assert_eq!(
        neighborhood["result"]["unresolved_name_matches"], 1,
        "{neighborhood}"
    );
    assert!(
        neighborhood["result"]["scope_excludes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "unresolved-static-calls"),
        "{neighborhood}"
    );
    assert!(
        neighborhood["result"]["next_action"]
            .as_str()
            .unwrap()
            .contains("entity_call_site_list"),
        "{neighborhood}"
    );

    let pack = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.target"}),
    )
    .await;
    assert_eq!(pack["ok"], true, "{pack}");
    let neighbors = &pack["result"]["neighbors"];
    assert_eq!(neighbors["unresolved_name_matches"], 1, "{pack}");
    assert!(
        neighbors["scope_excludes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "unresolved-static-calls"),
        "{pack}"
    );
    assert!(
        neighbors["next_action"]
            .as_str()
            .unwrap()
            .contains("entity_call_site_list"),
        "{pack}"
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
        conn.execute(
            "UPDATE runs SET analyzed_at_commit = ?1 WHERE id = 'run-fresh'",
            params!["abc123fresh"],
        )
        .expect("set analyzed commit");
    }
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(&state, "index_diff", json!({})).await;

    assert_eq!(envelope["ok"], true);
    let result = &envelope["result"];
    assert_eq!(result["overall"], "fresh");
    assert_eq!(result["drift_detected"], false);
    assert_eq!(result["analyzed_commit"], "abc123fresh");
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
         VALUES (?1,'loomweave','1.0',?2,'R1','defect','WARN',?3,'[]','m','{}','{}','[]','[]', \
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
        .find(|tool| tool.name == "project_status_get")
        .expect("project_status_get tool definition");
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
    conn.execute(
        "UPDATE runs SET analyzed_at_commit = ?1 WHERE id = 'run-1'",
        params!["abc123status"],
    )
    .expect("set analyzed commit");
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
    assert_eq!(result["latest_run"]["analyzed_at_commit"], "abc123status");
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
            .ends_with(".weft/loomweave/loomweave.db")
    );
    assert_eq!(result["git_sha"], "abc123status");
    // A bare ServerState carries no diagnostics context.
    assert_eq!(result["llm"], Value::Null);
    assert_eq!(result["filigree"], Value::Null);
}

#[tokio::test]
async fn project_status_emits_worktree_dirty_scope_note_on_every_path() {
    // N5: `worktree_dirty` is a bare boolean an agent (and legis, which gates
    // signing on it) reads as "git clean" on the false/null path. Emit a
    // consumer-visible scope note on EVERY path so the field's meaning —
    // un-indexed UNTRACKED source, not the git working-tree state — is readable
    // WITHOUT reading loomweave source. Here the project is not a git work tree,
    // so worktree_dirty is null, the path the note must still cover.
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let envelope = call_tool(&state, "project_status", json!({})).await;
    assert_eq!(envelope["ok"], true, "{envelope}");
    let note = envelope["result"]["worktree_dirty_note"]
        .as_str()
        .expect("worktree_dirty_note must be present on the null/false path");
    // The note must disclose that the field is NOT the git working-tree state and
    // that it is scoped to untracked source (modified tracked source surfaces via
    // staleness), so a signing gate doesn't read false as "git clean".
    let lower = note.to_lowercase();
    assert!(lower.contains("untracked"), "note: {note}");
    assert!(lower.contains("staleness"), "note: {note}");
    assert!(
        lower.contains("not"),
        "note must disclose it's not git-clean: {note}"
    );
}

#[tokio::test]
async fn project_status_fresh_carries_staleness_note_caveat() {
    // The named tool an agent reads directly must disclose what "fresh" omits —
    // not only the session-start banner (clarion-26c7e52027). The seeded demo.py
    // is older than a far-future run, so the verdict is Fresh.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    insert_run(
        &conn,
        "run-fresh",
        "2099-01-01T00:00:00.000Z",
        "completed",
        Some("2099-01-01T00:00:00.000Z"),
    );
    drop(conn);

    let state = state_for(project.path(), &db_path);
    let result = call_tool(&state, "project_status", json!({})).await["result"].clone();
    assert_eq!(
        result["staleness"], "fresh",
        "fixture must be fresh: {result}"
    );
    let note = result["staleness_note"]
        .as_str()
        .expect("a fresh verdict must carry a staleness_note");
    assert!(
        note.contains("loomweave analyze") && note.contains("index_diff_get"),
        "staleness_note must name index_diff_get as the authoritative surface (C-12) \
         and the re-analyze remedy: {note}"
    );
}

#[tokio::test]
async fn project_status_stale_note_defers_to_index_diff_by_name() {
    // C-12 (weft-4165f1ed71): a stale verdict points at the authoritative
    // surface for the per-signal detail. The seeded demo.py was just written
    // (mtime ~now), so a past-dated run makes the source newer than the run →
    // Stale, deterministically.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    insert_run(
        &conn,
        "run-1",
        "2026-02-02T00:00:00.000Z",
        "completed",
        Some("2026-02-02T00:00:00.000Z"),
    );
    drop(conn);

    let state = state_for(project.path(), &db_path);
    let result = call_tool(&state, "project_status", json!({})).await["result"].clone();
    assert_eq!(
        result["staleness"], "stale",
        "fixture must be stale: {result}"
    );
    let note = result["staleness_note"]
        .as_str()
        .expect("a stale verdict must defer to the authority by name");
    assert!(
        note.contains("index_diff_get"),
        "stale note must name the authoritative surface: {note}"
    );
}

#[tokio::test]
async fn project_status_reports_stale_worktree_for_untracked_source() {
    // The exact tool the dogfood report quoted (clarion-26c7e52027, ADR-045): a
    // mtime-fresh index in a git work tree that has a brand-new untracked module.
    // project_status_get must report staleness="stale_worktree" + worktree_dirty
    // = true, not a misleading bare "fresh".
    let (project, db_path) = open_project();

    // Make the project a git repo and commit everything seeded so far, so only
    // the new module below is untracked. Skip cleanly if git is unavailable.
    let git = |args: &[&str]| -> bool {
        std::process::Command::new("git")
            .args(args)
            .current_dir(project.path())
            .status()
            .is_ok_and(|s| s.success())
    };
    if !git(&["init", "-q"]) {
        return;
    }
    let _ = git(&["config", "user.email", "t@t"]);
    let _ = git(&["config", "user.name", "t"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);

    let conn = Connection::open(&db_path).expect("open sqlite");
    insert_run(
        &conn,
        "run-fresh",
        "2099-01-01T00:00:00.000Z",
        "completed",
        Some("2099-01-01T00:00:00.000Z"),
    );
    drop(conn);
    // Brand-new untracked Python module the index never saw.
    std::fs::write(project.path().join("hub.py"), "y = 2\n").expect("write untracked module");

    let state = state_for(project.path(), &db_path);
    let result = call_tool(&state, "project_status", json!({})).await["result"].clone();
    assert_eq!(
        result["staleness"], "stale_worktree",
        "untracked source must yield stale_worktree: {result}"
    );
    assert_eq!(
        result["worktree_dirty"], true,
        "worktree_dirty must be true: {result}"
    );
    assert!(
        result["staleness_note"]
            .as_str()
            .is_some_and(|n| n.contains("loomweave analyze")),
        "stale_worktree must carry a re-analyze note: {result}"
    );
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
async fn project_status_does_not_mutate_stale_running_run() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO runs ( \
            id, started_at, completed_at, config, stats, status, owner_pid, heartbeat_at \
         ) VALUES ( \
            'run-stale', '2026-02-04T00:00:00.000Z', NULL, '{}', '{}', \
            'running', 999999, '2000-01-01T00:00:00.000Z' \
         )",
        [],
    )
    .expect("insert stale running run");
    drop(conn);

    let state = state_for(project.path(), &db_path);
    let envelope = call_tool(&state, "project_status", json!({})).await;
    assert_eq!(envelope["ok"], true, "{envelope}");
    let latest = &envelope["result"]["latest_run"];
    assert_eq!(latest["id"], "run-stale");
    assert_eq!(latest["status"], "running");
    assert_eq!(latest["owner_pid"], 999_999);
    assert_eq!(latest["heartbeat_at"], "2000-01-01T00:00:00.000Z");
    assert_eq!(latest["completed_at"], Value::Null);

    let conn = Connection::open(&db_path).expect("reopen sqlite");
    let (run_status, completed_at, run_owner_pid, stats_json): (
        String,
        Option<String>,
        Option<i64>,
        String,
    ) = conn
        .query_row(
            "SELECT status, completed_at, owner_pid, stats FROM runs WHERE id = 'run-stale'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .expect("read run");
    assert_eq!(run_status, "running");
    assert_eq!(completed_at, None);
    assert_eq!(run_owner_pid, Some(999_999));
    let repair_stats: Value = serde_json::from_str(&stats_json).expect("stats json");
    assert_eq!(repair_stats, json!({}));
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
    // AC#3: the live ethereal port (.weft/filigree/ephemeral.port) is reported as
    // the resolution source, overriding the stale configured port.
    let (project, db_path) = open_project();
    let filigree_dir = project.path().join(".weft").join("filigree");
    fs::create_dir_all(&filigree_dir).unwrap();
    fs::write(filigree_dir.join("ephemeral.port"), "8542").unwrap();

    let config = FiligreeConfig {
        enabled: true,
        ..FiligreeConfig::default()
    };
    let diagnostics = DiagnosticsContext {
        llm: LlmDiagnostics {
            provider: "disabled".to_owned(),
            enabled: false,
            live: false,
            allow_live_provider: false,
            cache_max_age_days: 180,
        },
        filigree: resolve_filigree_url(&config, project.path(), |_| None),
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
            enabled: true,
            live: true,
            allow_live_provider: true,
            cache_max_age_days: 7,
        },
        filigree: resolve_filigree_url(&config, project.path(), |_| None),
    };
    let state = state_for(project.path(), &db_path).with_diagnostics(diagnostics);
    let envelope = call_tool(&state, "project_status", json!({})).await;
    let filigree = &envelope["result"]["filigree"];
    assert_eq!(filigree["resolved_url"], "http://127.0.0.1:8766");
    assert_eq!(filigree["resolution_source"], SOURCE_CONFIG);
    assert_eq!(envelope["result"]["llm"]["live"], true);
}

#[tokio::test]
async fn project_status_reports_loomweave_read_api_published_port() {
    // ADR-044: project_status surfaces the live read-API endpoint resolved from
    // .weft/loomweave/ephemeral.port (the second in-repo consumer of the resolver,
    // alongside doctor). No diagnostics context is needed — it resolves the
    // file at query time from the project root.
    let (project, db_path) = open_project();
    publish_port(project.path(), 9412).unwrap();

    let state = state_for(project.path(), &db_path);
    let envelope = call_tool(&state, "project_status", json!({})).await;
    let read_api = &envelope["result"]["loomweave_read_api"];
    assert_eq!(read_api["resolved_url"], "http://127.0.0.1:9412");
    assert_eq!(
        read_api["resolution_source"],
        LOOMWEAVE_SOURCE_EPHEMERAL_PORT
    );
}

#[tokio::test]
async fn project_status_loomweave_read_api_none_without_port_file() {
    // No published port file → resolution_source is "none" and resolved_url is
    // null (project_status has no static loomweave URL of its own).
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);
    let envelope = call_tool(&state, "project_status", json!({})).await;
    let read_api = &envelope["result"]["loomweave_read_api"];
    assert_eq!(read_api["resolved_url"], Value::Null);
    assert_eq!(read_api["resolution_source"], LOOMWEAVE_SOURCE_NONE);
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
async fn issues_for_normalizes_absolute_source_path_before_wardline_lookup() {
    let (project, db_path) = open_project();
    fs::create_dir_all(project.path().join("src")).expect("create src dir");
    let source_path = project.path().join("src/demo.py");
    fs::write(&source_path, "def hello():\n    pass\n").expect("write source file");
    let conn = Connection::open(&db_path).expect("open sqlite");
    conn.execute(
        "INSERT INTO entities (
            id, plugin_id, kind, name, short_name, parent_id, source_file_path,
            source_line_start, source_line_end, properties, content_hash,
            created_at, updated_at
         ) VALUES (
            'python:function:demo.hello', 'python', 'function',
            'python:function:demo.hello', 'demo.hello', NULL,
            ?1, 1, 3, '{}', 'fake-hash-wf-test',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [source_path.to_string_lossy().as_ref()],
    )
    .expect("insert demo.hello entity with absolute source path");
    drop(conn);

    let client = Arc::new(
        FakeFiligreeClient::default()
            .with_wardline_findings(vec![wf("demo.hello", "WLN-TAINT-001")]),
    );
    let state = state_for_filigree(project.path(), &db_path, client.clone());

    let envelope = call_tool(
        &state,
        "issues_for",
        json!({"id": "python:function:demo.hello", "include_contained": false}),
    )
    .await;

    assert_eq!(envelope["ok"], true);
    assert_eq!(
        client.wardline_path_calls(),
        vec!["src/demo.py".to_owned()],
        "Filigree path_prefix must be project-relative"
    );
    assert_eq!(
        envelope["result"]["wardline_findings"]["result_kind"],
        "matched"
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
    // section is `no_matches` with an empty items array — Loomweave never invents
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
// A4: entity dossier via `include` param on orientation_pack (clarion-2b87cd7a59)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn orientation_pack_dossier_absent_by_default() {
    // AC (regression): omitting `include` leaves the pack byte-identical — no
    // `dossier` key, not even null. The default path must not change.
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.entry"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp:?}");
    assert!(
        resp["result"].get("dossier").is_none(),
        "dossier must be absent when include is omitted: {resp:?}"
    );

    // An empty include array is also a no-op: no dossier.
    let empty = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.entry", "include": []}),
    )
    .await;
    assert!(
        empty["result"].get("dossier").is_none(),
        "dossier must be absent when include is an empty array: {empty:?}"
    );
    // Byte-identical to the omitted form.
    assert_eq!(resp, empty);
}

#[tokio::test]
async fn orientation_pack_dossier_with_all_sections() {
    // AC: include:["wardline","findings","issues"] folds in a `dossier` object
    // carrying all three sections plus the summary_available flag.
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
            'src/demo.py', 1, 3, '{}', 'fake-hash-dossier-all',
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
        json!({
            "entity": "python:function:demo.hello",
            "include": ["wardline", "findings", "issues"]
        }),
    )
    .await;
    assert_eq!(out["ok"], true, "{out:?}");

    let dossier = &out["result"]["dossier"];
    assert!(dossier.is_object(), "dossier object: {out:?}");
    assert!(
        dossier.get("wardline").is_some(),
        "dossier.wardline: {out:?}"
    );
    // The findings section mirrors its source tool's result object (as wardline
    // does): a `findings` array plus the `page` pagination metadata, so a caller
    // can detect truncation past the first page (clarion review P2).
    assert!(
        dossier["findings"]["findings"].is_array(),
        "dossier.findings.findings array: {out:?}"
    );
    assert!(
        dossier["findings"]["page"].is_object(),
        "dossier.findings.page must carry pagination metadata: {out:?}"
    );
    assert!(dossier.get("issues").is_some(), "dossier.issues: {out:?}");
    // No summary cached for this entity → flagged false; dossier still assembled.
    assert_eq!(dossier["summary_available"], false, "{out:?}");

    // Deterministic across re-runs.
    let again = call_tool(
        &state,
        "orientation_pack",
        json!({
            "entity": "python:function:demo.hello",
            "include": ["wardline", "findings", "issues"]
        }),
    )
    .await;
    assert_eq!(out, again);
}

#[tokio::test]
async fn orientation_pack_dossier_partial_include() {
    // AC: a single-section include yields only that key under dossier; the others
    // are absent (not null).
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let out = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.entry", "include": ["wardline"]}),
    )
    .await;
    assert_eq!(out["ok"], true, "{out:?}");
    let dossier = &out["result"]["dossier"];
    assert!(dossier.get("wardline").is_some(), "{out:?}");
    assert!(
        dossier.get("findings").is_none(),
        "findings must be absent for include:[wardline]: {out:?}"
    );
    assert!(
        dossier.get("issues").is_none(),
        "issues must be absent for include:[wardline]: {out:?}"
    );
    // summary_available is always present once dossier is built.
    assert!(dossier.get("summary_available").is_some(), "{out:?}");
}

#[tokio::test]
async fn orientation_pack_dossier_normalizes_fingerprints() {
    // AC: the in-house `61dc497…` vs `wlfp2:61dc497…` split is killed — every
    // fingerprint surfaced in the dossier is the bare canonical form. The opaque
    // Wardline taint blob (the source of dossier.wardline) carries a wlfp2:
    // fingerprint; the dossier must normalize it.
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
            'src/demo.py', 1, 3, '{}', 'fake-hash-dossier-fp',
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
            strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
         )",
        [],
    )
    .expect("insert demo.hello entity");
    upsert_taint_fact(
        &conn,
        &TaintFact {
            entity_id: "python:function:demo.hello".to_owned(),
            wardline_json: json!({
                "findings": [{
                    "rule_id": "WLN-TAINT-001",
                    "fingerprint": "wlfp2:61dc497abc"
                }]
            })
            .to_string(),
            scan_id: Some("scan-1".to_owned()),
            content_hash_at_compute: Some("fake-hash-dossier-fp".to_owned()),
            updated_at: "2026-01-01T00:00:00Z".to_owned(),
            sei: None,
        },
    )
    .expect("seed taint fact");
    drop(conn);

    let state = state_for(project.path(), &db_path);

    let out = call_tool(
        &state,
        "orientation_pack",
        json!({
            "entity": "python:function:demo.hello",
            "include": ["wardline", "findings", "issues"]
        }),
    )
    .await;
    assert_eq!(out["ok"], true, "{out:?}");

    let dossier_wf = &out["result"]["dossier"]["wardline"];
    let serialized = serde_json::to_string(dossier_wf).expect("serialize dossier wardline");
    assert!(
        !serialized.contains("wlfp2:"),
        "dossier.wardline must carry no wlfp2: prefix: {serialized}"
    );
    assert!(
        serialized.contains("61dc497abc"),
        "dossier.wardline must keep the canonical bare hash: {serialized}"
    );
}

#[tokio::test]
async fn orientation_pack_dossier_summary_available_true_when_cached() {
    // AC: summary_available is true exactly when a summary is cached for the
    // entity's current content_hash.
    let (project, db_path) = open_project();
    let content_hash = expected_content_hash(project.path(), "python:function:demo.entry");
    upsert_summary_cache(
        &Connection::open(&db_path).expect("open sqlite"),
        &SummaryCacheEntry {
            key: SummaryCacheKey {
                entity_id: "python:function:demo.entry".to_owned(),
                content_hash,
                prompt_template_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                model_tier: "anthropic/claude-sonnet-4.6".to_owned(),
                guidance_fingerprint: "guidance-empty".to_owned(),
            },
            summary_json: r#"{"purpose":"demo"}"#.to_owned(),
            cost_usd: 0.0,
            tokens_input: 0,
            tokens_output: 0,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            last_accessed_at: "2026-01-01T00:00:00Z".to_owned(),
            caller_count: 0,
            fan_out: 0,
            stale_semantic: false,
        },
    )
    .expect("upsert summary cache");

    let state = state_for(project.path(), &db_path);
    let out = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.entry", "include": ["findings"]}),
    )
    .await;
    assert_eq!(out["ok"], true, "{out:?}");
    assert_eq!(
        out["result"]["dossier"]["summary_available"], true,
        "summary_available must be true with a cached summary: {out:?}"
    );
}

#[tokio::test]
async fn orientation_pack_dossier_summary_available_false_on_stale_cache_key() {
    // Regression (clarion review P2): a cache row whose content_hash matches but
    // whose model_tier (or template / guidance) has since changed must NOT report
    // summary_available: true. entity_summary_get keys on the FULL SummaryCacheKey
    // and would miss this row, refetching — so a content-hash-only availability
    // check falsely tells a consult-mode caller it can skip generating.
    let (project, db_path) = open_project();
    let content_hash = expected_content_hash(project.path(), "python:function:demo.entry");
    upsert_summary_cache(
        &Connection::open(&db_path).expect("open sqlite"),
        &SummaryCacheEntry {
            key: SummaryCacheKey {
                entity_id: "python:function:demo.entry".to_owned(),
                content_hash,
                prompt_template_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                // Stale tier: the live read path resolves a different model id,
                // so the full-key lookup misses this row.
                model_tier: "anthropic/some-old-retired-model".to_owned(),
                guidance_fingerprint: "guidance-empty".to_owned(),
            },
            summary_json: r#"{"purpose":"demo"}"#.to_owned(),
            cost_usd: 0.0,
            tokens_input: 0,
            tokens_output: 0,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            last_accessed_at: "2026-01-01T00:00:00Z".to_owned(),
            caller_count: 0,
            fan_out: 0,
            stale_semantic: false,
        },
    )
    .expect("upsert summary cache");

    let state = state_for(project.path(), &db_path);
    let out = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.entry", "include": ["findings"]}),
    )
    .await;
    assert_eq!(out["ok"], true, "{out:?}");
    assert_eq!(
        out["result"]["dossier"]["summary_available"], false,
        "summary_available must be false when only content_hash matches a stale key: {out:?}"
    );
}

#[tokio::test]
async fn orientation_pack_dossier_findings_surface_pagination_truncation() {
    // Regression (clarion review P2): with more findings than the default page
    // size, the dossier's findings section must carry `page` metadata showing
    // truncation — otherwise include:["findings"] silently looks complete while
    // omitting everything past the first page.
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    insert_run(
        &conn,
        "run-findings",
        "2026-01-01T00:00:00.000Z",
        "completed",
        Some("2026-01-01T00:00:01.000Z"),
    );
    // 51 findings on the primary entity: one more than the 50-row default page.
    for i in 0..51 {
        insert_finding(
            &conn,
            &format!("F{i:03}"),
            "run-findings",
            "python:function:demo.entry",
        );
    }
    drop(conn);

    let state = state_for(project.path(), &db_path);
    let out = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:function:demo.entry", "include": ["findings"]}),
    )
    .await;
    assert_eq!(out["ok"], true, "{out:?}");

    let findings = &out["result"]["dossier"]["findings"];
    let page = &findings["page"];
    assert_eq!(
        page["total"], 51,
        "page.total must reflect all findings: {out:?}"
    );
    assert_eq!(
        page["truncated"], true,
        "page.truncated must flag the dropped tail: {out:?}"
    );
    assert_eq!(
        findings["findings"].as_array().map(Vec::len),
        Some(50),
        "first page returns the 50-row default: {out:?}"
    );
}

#[tokio::test]
async fn orientation_pack_dossier_reports_delegated_tool_error_envelopes() {
    let (project, db_path) = open_project();
    {
        let conn = Connection::open(&db_path).expect("open sqlite");
        conn.execute_batch(
            "DROP TABLE findings;
             DROP TABLE wardline_taint_facts;",
        )
        .expect("drop delegated-read tables");
    }
    let state = state_for(project.path(), &db_path);

    let out = call_tool(
        &state,
        "orientation_pack",
        json!({
            "entity": "python:function:demo.entry",
            "include": ["wardline", "findings"]
        }),
    )
    .await;

    assert_eq!(out["ok"], true, "{out:?}");
    let dossier = &out["result"]["dossier"];
    assert_eq!(dossier["wardline"]["available"], false, "{out:?}");
    assert_eq!(
        dossier["wardline"]["reason"], "wardline lookup failed",
        "{out:?}"
    );
    assert!(
        dossier["wardline"]["error"].is_object(),
        "wardline error envelope must be preserved: {out:?}"
    );
    assert_eq!(dossier["findings"]["available"], false, "{out:?}");
    assert_eq!(
        dossier["findings"]["reason"], "findings lookup failed",
        "{out:?}"
    );
    assert!(
        dossier["findings"]["error"].is_object(),
        "findings error envelope must be preserved: {out:?}"
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
    // verbatim; Loomweave does not selectively strip or rename fields.
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

// ---------------------------------------------------------------------------
// Item 1 (clarion-d76e7f7267): id-taking tools accept a SEI and resolve it to
// the SAME entity as the locator. Each test seeds the alive binding
// loomweave:eid:demo-entry -> python:function:demo.entry, then asserts a call
// keyed by the SEI matches a call keyed by the locator.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn callers_of_accepts_sei_and_resolves_to_same_entity_as_locator() {
    let (project, db_path) = open_project();
    seed_alive_sei_binding(
        &db_path,
        "loomweave:eid:demo-entry",
        "python:function:demo.mid",
    );
    let state = state_for(project.path(), &db_path);

    let by_locator = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.mid"}),
    )
    .await;
    let by_sei = call_tool(
        &state,
        "callers_of",
        json!({"id": "loomweave:eid:demo-entry"}),
    )
    .await;

    assert_eq!(by_locator["ok"], true, "{by_locator}");
    assert_eq!(by_sei["ok"], true, "{by_sei}");
    assert_eq!(
        by_sei["result"]["callers"], by_locator["result"]["callers"],
        "SEI-keyed callers must equal locator-keyed callers"
    );
    assert_eq!(
        by_sei["result"]["callers"][0]["entity"]["id"],
        "python:function:demo.entry"
    );
}

#[tokio::test]
async fn neighborhood_accepts_sei_and_resolves_to_same_entity_as_locator() {
    let (project, db_path) = open_project();
    seed_alive_sei_binding(
        &db_path,
        "loomweave:eid:demo-entry",
        "python:function:demo.entry",
    );
    let state = state_for(project.path(), &db_path);

    let by_locator = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    let by_sei = call_tool(
        &state,
        "neighborhood",
        json!({"id": "loomweave:eid:demo-entry"}),
    )
    .await;

    assert_eq!(by_locator["ok"], true, "{by_locator}");
    assert_eq!(by_sei["ok"], true, "{by_sei}");
    assert_eq!(
        by_sei["result"]["entity"]["id"],
        "python:function:demo.entry"
    );
    assert_eq!(by_sei["result"]["callees"], by_locator["result"]["callees"]);
}

#[tokio::test]
async fn summary_accepts_sei_and_resolves_to_same_entity_as_locator() {
    let (project, db_path) = open_project();
    seed_alive_sei_binding(
        &db_path,
        "loomweave:eid:demo-entry",
        "python:function:demo.entry",
    );
    // No LLM configured: both resolve to the same entity and return the same
    // llm-disabled envelope rather than EntityNotFound (the SEI was accepted).
    let state =
        state_for(project.path(), &db_path).with_tool_policy(McpToolPolicy::allow_write_tools());

    let by_locator = call_tool(
        &state,
        "summary",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    let by_sei = call_tool(&state, "summary", json!({"id": "loomweave:eid:demo-entry"})).await;

    assert_eq!(by_locator["error"]["code"], "llm-disabled", "{by_locator}");
    assert_eq!(
        by_sei["error"]["code"], "llm-disabled",
        "a seeded SEI must resolve to the same llm-disabled outcome, not 404: {by_sei}"
    );
}

#[tokio::test]
async fn source_for_entity_accepts_sei_and_resolves_to_same_entity_as_locator() {
    let (project, db_path) = open_project();
    seed_alive_sei_binding(
        &db_path,
        "loomweave:eid:demo-entry",
        "python:function:demo.entry",
    );
    let state = state_for(project.path(), &db_path);

    let by_locator = call_tool(
        &state,
        "source_for_entity",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    let by_sei = call_tool(
        &state,
        "source_for_entity",
        json!({"id": "loomweave:eid:demo-entry"}),
    )
    .await;

    assert_eq!(by_locator["ok"], true, "{by_locator}");
    assert_eq!(by_sei["ok"], true, "{by_sei}");
    assert_eq!(by_sei["result"], by_locator["result"]);
}

#[tokio::test]
async fn call_sites_accepts_sei_and_resolves_to_same_entity_as_locator() {
    // The new existence gate (Landmine #3): call_sites had no gate, so a SEI
    // would silently return empty. Assert SEI == locator here, and the
    // orphan/unknown-SEI -> NotFound case in the dedicated test below.
    let (project, db_path) = open_project();
    seed_alive_sei_binding(
        &db_path,
        "loomweave:eid:demo-entry",
        "python:function:demo.entry",
    );
    let state = state_for(project.path(), &db_path);

    let by_locator = call_tool(
        &state,
        "call_sites",
        json!({"id": "python:function:demo.entry"}),
    )
    .await;
    let by_sei = call_tool(
        &state,
        "call_sites",
        json!({"id": "loomweave:eid:demo-entry"}),
    )
    .await;

    assert_eq!(by_locator["ok"], true, "{by_locator}");
    assert_eq!(
        by_sei["ok"], true,
        "SEI must resolve, not silent-empty: {by_sei}"
    );
    assert_eq!(by_sei["result"], by_locator["result"]);
}

#[tokio::test]
async fn call_sites_unknown_sei_is_not_found_not_empty() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let env = call_tool(
        &state,
        "call_sites",
        json!({"id": "loomweave:eid:does-not-exist"}),
    )
    .await;

    assert_eq!(
        env["ok"], false,
        "unknown SEI must be an error, not empty: {env}"
    );
    assert_eq!(env["error"]["code"], "not-found");
}

#[tokio::test]
async fn id_taking_tool_rejects_orphaned_sei_as_entity_not_found() {
    // A SEI that resolves to NotAlive (no binding seeded) must fail closed:
    // resolve_entity_ref returns None -> EntityNotFound.
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let env = call_tool(
        &state,
        "callers_of",
        json!({"id": "loomweave:eid:orphaned-unknown"}),
    )
    .await;

    assert_eq!(env["ok"], false, "{env}");
    assert_eq!(env["error"]["code"], "entity-not-found");
    assert!(
        env["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("loomweave:eid:orphaned-unknown"),
        "EntityNotFound echoes the pasted SEI: {env}"
    );
}

#[tokio::test]
async fn find_entity_with_pasted_sei_returns_the_resolved_entity() {
    // Item 1.E: a pasted SEI exact-resolves (was empty before — the SEI lives
    // only in sei_bindings, never in any entities column).
    let (project, db_path) = open_project();
    seed_alive_sei_binding(
        &db_path,
        "loomweave:eid:demo-entry",
        "python:function:demo.entry",
    );
    let state = state_for(project.path(), &db_path);

    let env = call_tool(
        &state,
        "find_entity",
        json!({"pattern": "loomweave:eid:demo-entry"}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    let entities = env["result"]["entities"]
        .as_array()
        .expect("entities array");
    assert_eq!(
        entities.len(),
        1,
        "pasted SEI exact-resolves to one row: {env}"
    );
    assert_eq!(entities[0]["id"], "python:function:demo.entry");
}

#[tokio::test]
async fn find_entity_with_unknown_sei_returns_empty_not_error() {
    let (project, db_path) = open_project();
    let state = state_for(project.path(), &db_path);

    let env = call_tool(
        &state,
        "find_entity",
        json!({"pattern": "loomweave:eid:nope"}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    assert_eq!(
        env["result"]["entities"]
            .as_array()
            .expect("entities")
            .len(),
        0
    );
}

#[tokio::test]
async fn propose_guidance_accepts_sei_but_stores_the_locator_in_match_rule() {
    // Landmine #1: a SEI may be accepted, but the default match_rule and the
    // proposal's entity_id MUST carry the resolved LOCATOR, not the raw SEI —
    // otherwise the stored guidance silently becomes SEI-keyed.
    let (project, db_path) = open_project();
    seed_alive_sei_binding(
        &db_path,
        "loomweave:eid:demo-entry",
        "python:function:demo.entry",
    );
    let client = Arc::new(FakeFiligreeClient::default());
    let state = state_for_filigree(project.path(), &db_path, client.clone());

    let proposed = call_tool(
        &state,
        "propose_guidance",
        json!({
            "entity_id": "loomweave:eid:demo-entry",
            "content": "Locator-keyed guidance even when proposed by SEI.",
            "scope_level": "function"
        }),
    )
    .await;

    assert_eq!(proposed["ok"], true, "{proposed}");
    let created = client.created_observations();
    assert_eq!(created.len(), 1);
    let proposal =
        GuidanceProposal::from_observation_detail(&created[0].detail).expect("parse proposal");
    assert_eq!(
        proposal.entity_id, "python:function:demo.entry",
        "proposal entity_id must be the resolved locator, not the SEI"
    );
    assert_eq!(
        proposal.match_rules[0]["id"], "python:function:demo.entry",
        "default match_rule id must be the locator, not the SEI: {:?}",
        proposal.match_rules
    );
}

// ---------------------------------------------------------------------------
// Item 3 (clarion-d76e7f7267): bounded graph relationship tools.
//   * Single-relation (callers_of, subsystem_members): limit + cursor +
//     next_cursor + explicit truncated.
//   * Neighborhood: ONE per-bucket limit + a truncated MAP, NO cursor.
// ---------------------------------------------------------------------------

/// Seed `count` distinct functions that all call `python:function:demo.target`,
/// so `callers_of(demo.target)` has a deterministic, paginable caller set.
fn seed_extra_callers(db_path: &std::path::Path, count: usize) {
    let conn = Connection::open(db_path).expect("open sqlite");
    let source_path: String = conn
        .query_row(
            "SELECT source_file_path FROM entities WHERE id = 'python:function:demo.target'",
            [],
            |row| row.get(0),
        )
        .expect("target source path");
    for i in 0..count {
        let id = format!("python:function:demo.caller{i}");
        conn.execute(
            "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_path, \
                source_line_start, source_line_end, properties, content_hash, created_at, updated_at) \
             VALUES (?1,'python','function',?1,?1,?2,1,2,'{}','hash','2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
            params![id, source_path],
        )
        .expect("insert caller entity");
        conn.execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence, source_byte_start, source_byte_end) \
             VALUES ('calls', ?1, 'python:function:demo.target', 'resolved', 10, 20)",
            params![id],
        )
        .expect("insert calls edge");
    }
}

#[tokio::test]
async fn callers_of_paginates_with_limit_cursor_and_truncated() {
    let (project, db_path) = open_project();
    // demo.mid already calls demo.target (resolved). Add 2 more -> 3 callers.
    seed_extra_callers(&db_path, 2);
    let state = state_for(project.path(), &db_path);

    let first = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.target", "limit": 2}),
    )
    .await;
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["result"]["callers"].as_array().unwrap().len(), 2);
    assert_eq!(first["result"]["next_cursor"], "2");
    assert_eq!(
        first["result"]["truncated"], true,
        "first page of 3 with limit 2 must be truncated"
    );

    let second = call_tool(
        &state,
        "callers_of",
        json!({"id": "python:function:demo.target", "limit": 2, "cursor": "2"}),
    )
    .await;
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(second["result"]["callers"].as_array().unwrap().len(), 1);
    assert_eq!(second["result"]["next_cursor"], Value::Null);
    assert_eq!(second["result"]["truncated"], false);
}

#[tokio::test]
async fn subsystem_members_paginates_with_limit_cursor_and_truncated() {
    let (project, db_path) = open_project();
    let conn = Connection::open(&db_path).expect("open sqlite");
    let subsystem_id = seed_subsystem(&conn, project.path()); // 2 members
    drop(conn);
    let state = state_for(project.path(), &db_path);

    let first = call_tool(
        &state,
        "subsystem_members",
        json!({"id": subsystem_id, "limit": 1}),
    )
    .await;
    assert_eq!(first["ok"], true, "{first}");
    assert_eq!(first["result"]["members"].as_array().unwrap().len(), 1);
    assert_eq!(first["result"]["next_cursor"], "1");
    assert_eq!(first["result"]["truncated"], true);

    let second = call_tool(
        &state,
        "subsystem_members",
        json!({"id": subsystem_id, "limit": 1, "cursor": "1"}),
    )
    .await;
    assert_eq!(second["ok"], true, "{second}");
    assert_eq!(second["result"]["members"].as_array().unwrap().len(), 1);
    assert_eq!(second["result"]["next_cursor"], Value::Null);
    assert_eq!(second["result"]["truncated"], false);
}

#[tokio::test]
async fn neighborhood_caps_each_bucket_and_reports_a_truncated_map_with_no_cursor() {
    let (project, db_path) = open_project();
    // demo.target's callers: demo.mid plus 4 seeded -> 5 inbound callers.
    seed_extra_callers(&db_path, 4);
    let state = state_for(project.path(), &db_path);

    let env = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:demo.target", "limit": 2}),
    )
    .await;

    assert_eq!(env["ok"], true, "{env}");
    // The callers bucket is capped at the per-bucket limit and flagged truncated.
    assert_eq!(
        env["result"]["callers"].as_array().unwrap().len(),
        2,
        "callers bucket must be capped at limit: {env}"
    );
    assert_eq!(
        env["result"]["truncated"]["callers"], true,
        "the truncated MAP must flag the trimmed callers bucket: {env}"
    );
    // A bucket under the cap is NOT flagged truncated.
    assert_eq!(env["result"]["truncated"]["imports_out"], false);
    // The overview has NO cursor — one cursor cannot advance 7 buckets.
    assert!(
        env["result"].get("next_cursor").is_none(),
        "neighborhood overview must NOT carry a cursor: {env}"
    );
}

// ---------------------------------------------------------------------------
// entity_relation_list (clarion-ae5b43ea40): the relation kinds
// (inherits_from / decorates / implements / derives) were write-only — no MCP
// read path served them. Direction semantics and the decorates anchor-file
// inversion are pinned by ADR-051.
// ---------------------------------------------------------------------------

/// Seed a relation fixture on top of [`open_project`]'s graph: a class
/// hierarchy in types.py and a decorated handler in app.py, with the
/// `decorates` anchor living in the *decorated* side's file (ADR-051).
fn seed_relation_fixture(project_root: &std::path::Path, db_path: &std::path::Path) {
    let conn = Connection::open(db_path).expect("open sqlite");
    let types_path = project_root.join("types.py");
    std::fs::write(
        &types_path,
        "class Base:\n    pass\n\nclass Child(Base):\n    pass\n\ndef wrap(fn):\n    return fn\n",
    )
    .expect("write types source");
    let app_path = project_root.join("app.py");
    std::fs::write(&app_path, "@wrap\ndef handler():\n    return 1\n").expect("write app source");

    insert_file_entity(&conn, "core:file:types.py", &types_path);
    insert_file_entity(&conn, "core:file:app.py", &app_path);
    insert_entity(
        &conn,
        "python:module:types",
        "module",
        &types_path,
        Some((1, 8)),
        None,
    );
    insert_entity(
        &conn,
        "python:class:types.Base",
        "class",
        &types_path,
        Some((1, 2)),
        Some("python:module:types"),
    );
    insert_entity(
        &conn,
        "python:class:types.Child",
        "class",
        &types_path,
        Some((4, 5)),
        Some("python:module:types"),
    );
    insert_entity(
        &conn,
        "python:function:types.wrap",
        "function",
        &types_path,
        Some((7, 8)),
        Some("python:module:types"),
    );
    insert_entity(
        &conn,
        "python:module:app",
        "module",
        &app_path,
        Some((1, 3)),
        None,
    );
    insert_entity(
        &conn,
        "python:function:app.handler",
        "function",
        &app_path,
        Some((1, 3)),
        Some("python:module:app"),
    );

    // "class Child(Base):" — the `Base` token spans bytes 34..38 of types.py
    // (line 4, byte column 12).
    insert_relation_edge_row(
        &conn,
        "inherits_from",
        "python:class:types.Child",
        "python:class:types.Base",
        "resolved",
        None,
        "core:file:types.py",
        34,
        38,
    );
    // "@wrap" — the `wrap` token spans bytes 1..5 of app.py (line 1, byte
    // column 1). The anchor file is the DECORATED side's file.
    insert_relation_edge_row(
        &conn,
        "decorates",
        "python:function:types.wrap",
        "python:function:app.handler",
        "resolved",
        None,
        "core:file:app.py",
        1,
        5,
    );
}

#[allow(clippy::too_many_arguments)] // a full anchored edge row IS this wide
fn insert_relation_edge_row(
    conn: &Connection,
    kind: &str,
    from_id: &str,
    to_id: &str,
    confidence: &str,
    properties: Option<Value>,
    source_file_id: &str,
    byte_start: i64,
    byte_end: i64,
) {
    conn.execute(
        "INSERT INTO edges (
            kind, from_id, to_id, confidence, properties, source_file_id,
            source_byte_start, source_byte_end
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            kind,
            from_id,
            to_id,
            confidence,
            properties.map(|value| value.to_string()),
            source_file_id,
            byte_start,
            byte_end,
        ],
    )
    .expect("insert relation edge");
}

#[tokio::test]
async fn relation_list_answers_directional_queries_with_anchor_evidence() {
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    let state = state_for(project.path(), &db_path);

    // "What subclasses Base" — a TO-side (direction=in) lookup on inherits_from.
    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base", "direction": "in"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    assert_eq!(resp["result"]["entity"]["id"], "python:class:types.Base");
    assert_eq!(resp["result"]["direction"], "in");
    let relations = resp["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{resp}");
    let rel = &relations[0];
    assert_eq!(rel["kind"], "inherits_from");
    assert_eq!(rel["entity"]["id"], "python:class:types.Child");
    assert_eq!(rel["edge_confidence"], "resolved");
    assert_eq!(
        rel["file"],
        project.path().join("types.py").display().to_string()
    );
    assert_eq!(rel["line"], 4);
    assert_eq!(rel["column"], 12);
    assert_eq!(rel["line_text"], "class Child(Base):");
    assert_eq!(rel["byte_start"], 34);
    assert_eq!(rel["byte_end"], 38);
    assert_eq!(resp["result"]["next_cursor"], Value::Null);
    assert_eq!(resp["result"]["truncated"], false);

    // "What does wrap decorate" — direction=out on the DECORATOR (ADR-051:
    // decorates runs decorator → decorated). The anchor evidence must follow
    // the edge's own file (app.py, the decorated side), not the queried
    // entity's file.
    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:function:types.wrap", "direction": "out"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    let relations = resp["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{resp}");
    let rel = &relations[0];
    assert_eq!(rel["kind"], "decorates");
    assert_eq!(rel["entity"]["id"], "python:function:app.handler");
    assert_eq!(
        rel["file"],
        project.path().join("app.py").display().to_string(),
        "decorates anchor must come from the edge's file, not the decorator's: {rel}"
    );
    assert_eq!(rel["line"], 1);
    assert_eq!(rel["column"], 1);
    assert_eq!(rel["line_text"], "@wrap");

    // "What decorates handler" — direction=in on the decorated entity.
    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:function:app.handler", "direction": "in"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    let relations = resp["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{resp}");
    assert_eq!(relations[0]["kind"], "decorates");
    assert_eq!(relations[0]["entity"]["id"], "python:function:types.wrap");

    // The subclass side: direction=out on Child names its base.
    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Child", "direction": "out"}),
    )
    .await;
    let relations = resp["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{resp}");
    assert_eq!(relations[0]["entity"]["id"], "python:class:types.Base");
}

#[tokio::test]
async fn relation_list_both_direction_unions_in_and_out() {
    // A2 (clarion-057ff2b330): direction="both" returns the in+out union, each
    // relation tagged with its own direction. Seed types.Child with BOTH a
    // superclass (Base, out) and a subclass (Grand, in) so the union is visible.
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    {
        let conn = Connection::open(&db_path).expect("open sqlite");
        let types_path = project.path().join("types.py");
        insert_entity(
            &conn,
            "python:class:types.Grand",
            "class",
            &types_path,
            Some((4, 5)),
            Some("python:module:types"),
        );
        insert_relation_edge_row(
            &conn,
            "inherits_from",
            "python:class:types.Grand",
            "python:class:types.Child",
            "resolved",
            None,
            "core:file:types.py",
            34,
            38,
        );
    }
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Child", "direction": "both"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    assert_eq!(resp["result"]["direction"], "both");
    let relations = resp["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 2, "both must union in+out: {resp}");

    let in_rel = relations
        .iter()
        .find(|r| r["direction"] == "in")
        .expect("an in relation");
    assert_eq!(in_rel["entity"]["id"], "python:class:types.Grand");
    let out_rel = relations
        .iter()
        .find(|r| r["direction"] == "out")
        .expect("an out relation");
    assert_eq!(out_rel["entity"]["id"], "python:class:types.Base");
}

#[tokio::test]
async fn relation_list_omitted_direction_defaults_to_both() {
    // A2: omitting direction is NOT an error — it behaves as "both".
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    assert_eq!(resp["result"]["direction"], "both");
    // Base has one subclass (Child, in) and no superclass (out), so the union is
    // exactly the single in relation.
    let relations = resp["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{resp}");
    assert_eq!(relations[0]["direction"], "in");
    assert_eq!(relations[0]["entity"]["id"], "python:class:types.Child");
}

#[tokio::test]
async fn relation_list_kind_filter_pagination_and_honest_empty() {
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    {
        let conn = Connection::open(&db_path).expect("open sqlite");
        let types_path = project.path().join("types.py");
        insert_entity(
            &conn,
            "python:class:types.Other",
            "class",
            &types_path,
            Some((4, 5)),
            Some("python:module:types"),
        );
        insert_relation_edge_row(
            &conn,
            "inherits_from",
            "python:class:types.Other",
            "python:class:types.Base",
            "resolved",
            None,
            "core:file:types.py",
            34,
            38,
        );
    }
    let state = state_for(project.path(), &db_path);

    // limit=1 pages the two subclasses deterministically (Child before Other).
    let first = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base", "direction": "in", "limit": 1}),
    )
    .await;
    assert_eq!(first["ok"], true, "{first}");
    let relations = first["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{first}");
    assert_eq!(relations[0]["entity"]["id"], "python:class:types.Child");
    assert_eq!(first["result"]["truncated"], true, "{first}");
    assert_eq!(first["result"]["next_cursor"], "1", "{first}");

    let second = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base", "direction": "in", "limit": 1, "cursor": "1"}),
    )
    .await;
    let relations = second["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{second}");
    assert_eq!(relations[0]["entity"]["id"], "python:class:types.Other");
    assert_eq!(second["result"]["truncated"], false, "{second}");
    assert_eq!(second["result"]["next_cursor"], Value::Null, "{second}");

    // A kind filter that matches nothing is honest-empty, not an error.
    let none = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base", "direction": "in", "kind": "decorates"}),
    )
    .await;
    assert_eq!(none["ok"], true, "{none}");
    assert!(
        none["result"]["relations"].as_array().unwrap().is_empty(),
        "{none}"
    );
    assert_eq!(none["result"]["truncated"], false, "{none}");

    // The kind filter narrows to the requested kind only.
    let filtered = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base", "direction": "in", "kind": "inherits_from"}),
    )
    .await;
    let filtered_rows = filtered["result"]["relations"].as_array().unwrap();
    assert_eq!(filtered_rows.len(), 2, "{filtered}");
    assert!(
        filtered_rows.iter().all(|r| r["kind"] == "inherits_from"),
        "kind filter must narrow to the requested kind only: {filtered}"
    );
}

#[tokio::test]
async fn relation_list_confidence_gates_ambiguous_and_passes_candidates() {
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    {
        let conn = Connection::open(&db_path).expect("open sqlite");
        let app_path = project.path().join("app.py");
        let types_path = project.path().join("types.py");
        insert_entity(
            &conn,
            "python:function:app.other",
            "function",
            &app_path,
            Some((2, 3)),
            Some("python:module:app"),
        );
        // Candidates pass through only when they resolve to VISIBLE entities
        // (relation_list_candidates_never_leak_blocked_ids), so the
        // alternative candidate needs a real row.
        insert_entity(
            &conn,
            "python:function:types.wrap_again",
            "function",
            &types_path,
            Some((7, 8)),
            Some("python:module:types"),
        );
        // An ambiguous decorates edge: the FROM side is the best-guess
        // decorator and `candidates` are alternative FROM-side ids (ADR-051's
        // inverted-candidates trap).
        insert_relation_edge_row(
            &conn,
            "decorates",
            "python:function:types.wrap",
            "python:function:app.other",
            "ambiguous",
            Some(json!({"candidates": [
                "python:function:types.wrap",
                "python:function:types.wrap_again"
            ]})),
            "core:file:app.py",
            1,
            5,
        );
    }
    let state = state_for(project.path(), &db_path);

    // Default confidence (resolved) excludes the ambiguous edge.
    let resolved = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:function:app.other", "direction": "in"}),
    )
    .await;
    assert_eq!(resolved["ok"], true, "{resolved}");
    assert!(
        resolved["result"]["relations"]
            .as_array()
            .unwrap()
            .is_empty(),
        "resolved tier must exclude ambiguous relation edges: {resolved}"
    );

    // Opting into ambiguous surfaces it, with the candidate ids passed through.
    let ambiguous = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:function:app.other", "direction": "in", "confidence": "ambiguous"}),
    )
    .await;
    let relations = ambiguous["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{ambiguous}");
    assert_eq!(relations[0]["edge_confidence"], "ambiguous");
    assert_eq!(
        relations[0]["candidates"],
        json!([
            "python:function:types.wrap",
            "python:function:types.wrap_again"
        ]),
        "{ambiguous}"
    );
    // Resolved entries carry no candidates.
    let base = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base", "direction": "in"}),
    )
    .await;
    assert!(
        base["result"]["relations"][0]["candidates"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{base}"
    );
}

#[tokio::test]
async fn relation_list_re_withholds_secretlike_blocked_candidate_ids() {
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    let secret = "fn_aGVsbG8gd29ybGQgc2VjcmV0IGtleSBhYmMxMjP8x9z";
    let secret_id = format!("python:function:{secret}");
    {
        let conn = Connection::open(&db_path).expect("open sqlite");
        let app_path = project.path().join("app.py");
        let types_path = project.path().join("types.py");
        insert_entity(
            &conn,
            "python:function:app.other",
            "function",
            &app_path,
            Some((2, 3)),
            Some("python:module:app"),
        );
        insert_entity(
            &conn,
            &secret_id,
            "function",
            &types_path,
            Some((7, 8)),
            Some("python:module:types"),
        );
        insert_relation_edge_row(
            &conn,
            "decorates",
            "python:function:types.wrap",
            "python:function:app.other",
            "ambiguous",
            Some(json!({"candidates": [secret_id]})),
            "core:file:app.py",
            1,
            5,
        );
    }
    mark_blocked(&db_path, &secret_id, "secret_present");
    let state = state_for(project.path(), &db_path);

    let ambiguous = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:function:app.other", "direction": "in", "confidence": "ambiguous"}),
    )
    .await;

    assert_eq!(ambiguous["ok"], true, "{ambiguous}");
    let relation = &ambiguous["result"]["relations"].as_array().unwrap()[0];
    assert_eq!(
        relation["candidates"],
        json!([Value::Null]),
        "secretlike blocked candidate id must be re-withheld: {ambiguous}"
    );
    assert!(
        !ambiguous.to_string().contains(secret),
        "secretlike relation candidate leaked: {ambiguous}"
    );
}

#[tokio::test]
async fn relation_list_resolves_sei_and_reports_unknown_id() {
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    seed_alive_sei_binding(
        &db_path,
        "loomweave:eid:types-base",
        "python:class:types.Base",
    );
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "loomweave:eid:types-base", "direction": "in"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    let relations = resp["result"]["relations"].as_array().unwrap();
    assert_eq!(
        relations.len(),
        1,
        "SEI input must resolve to its locator: {resp}"
    );
    assert_eq!(relations[0]["entity"]["id"], "python:class:types.Child");

    let missing = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Ghost", "direction": "in"}),
    )
    .await;
    assert_eq!(missing["ok"], false, "{missing}");
    assert_eq!(missing["error"]["code"], "entity-not-found", "{missing}");
}

#[tokio::test]
async fn relation_list_refuses_structure_for_blocked_entity() {
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    mark_blocked(&db_path, "python:class:types.Base", "secret_present");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base", "direction": "in"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    assert_eq!(resp["result"]["available"], false, "{resp}");
    assert_eq!(
        resp["result"]["briefing_blocked"], "secret_present",
        "{resp}"
    );
    assert!(resp["result"]["relations"].is_null(), "{resp}");
    assert!(
        !resp.to_string().contains("types.Base"),
        "blocked id leaked in relation_list refusal: {resp}"
    );
}

#[tokio::test]
async fn relation_list_redacts_blocked_neighbor_and_blocked_anchor_file() {
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    // Block the subclass: querying Base must stub the neighbor AND withhold
    // the line evidence (the line text contains the blocked declaration).
    mark_blocked(&db_path, "python:class:types.Child", "secret_present");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base", "direction": "in"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    let relations = resp["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{resp}");
    let rel = &relations[0];
    // Under A3 the blocked neighbor keeps its navigable identity…
    assert_blocked_identity_present(&rel["entity"], "secret_present");
    assert_eq!(rel["entity"]["id"], "python:class:types.Child", "{rel}");
    // …but the source-line *evidence* (the bytes behind the anchor) stays
    // withheld — that is the secret content, not the identity.
    assert_eq!(rel["source_status"], "briefing_blocked", "{rel}");
    assert!(rel["line"].is_null(), "{rel}");
    assert_eq!(rel["line_text"], "", "{rel}");

    // Blocking the anchor FILE entity withholds line evidence even when both
    // endpoints are visible (the bytes behind the anchor are scanner-withheld).
    mark_blocked(&db_path, "core:file:app.py", "secret_present");
    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:function:types.wrap", "direction": "out"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    let rel = &resp["result"]["relations"].as_array().unwrap()[0];
    assert_eq!(rel["entity"]["id"], "python:function:app.handler", "{rel}");
    assert_eq!(rel["source_status"], "briefing_blocked", "{rel}");
    assert!(rel["line"].is_null(), "{rel}");
    assert_eq!(rel["line_text"], "", "{rel}");
}

#[test]
fn tools_list_includes_entity_relation_list() {
    let tools = list_tools();
    let tool = tools
        .iter()
        .find(|tool| tool.name == "entity_relation_list")
        .expect("entity_relation_list tool definition");
    assert_eq!(
        tool.input_schema,
        json!({
            "type": "object",
            "properties": {
                "id": {"type": "string", "minLength": 1},
                "direction": {"type": "string", "enum": ["in", "out", "both"]},
                "kind": {
                    "type": "string",
                    "enum": ["inherits_from", "decorates", "implements", "derives"]
                },
                "confidence": {
                    "type": "string",
                    "enum": ["resolved", "ambiguous", "inferred"],
                    "default": "resolved"
                },
                "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                "cursor": {"type": ["string", "null"]}
            },
            "required": ["id"],
            "additionalProperties": false
        })
    );
}

#[tokio::test]
async fn neighborhood_lists_relation_buckets() {
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    let state = state_for(project.path(), &db_path);

    // A class with an inbound inherits_from: relations_in names the subclass,
    // tagged with its kind; relations_out is honest-empty.
    let resp = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:class:types.Base"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    let relations_in = resp["result"]["relations_in"].as_array().unwrap();
    assert_eq!(relations_in.len(), 1, "{resp}");
    assert_eq!(relations_in[0]["kind"], "inherits_from");
    assert_eq!(relations_in[0]["entity"]["id"], "python:class:types.Child");
    assert_eq!(relations_in[0]["edge_confidence"], "resolved");
    assert!(
        resp["result"]["relations_out"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{resp}"
    );
    // The truncated map covers the new buckets.
    assert_eq!(resp["result"]["truncated"]["relations_in"], false, "{resp}");
    assert_eq!(
        resp["result"]["truncated"]["relations_out"], false,
        "{resp}"
    );

    // The decorator side: relations_out names what it decorates (ADR-051
    // direction — the decorator is the FROM side).
    let resp = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:types.wrap"}),
    )
    .await;
    let relations_out = resp["result"]["relations_out"].as_array().unwrap();
    assert_eq!(relations_out.len(), 1, "{resp}");
    assert_eq!(relations_out[0]["kind"], "decorates");
    assert_eq!(
        relations_out[0]["entity"]["id"],
        "python:function:app.handler"
    );

    // The per-bucket limit trims and flags relation buckets like any other.
    {
        let conn = Connection::open(&db_path).expect("open sqlite");
        let types_path = project.path().join("types.py");
        insert_entity(
            &conn,
            "python:class:types.Other",
            "class",
            &types_path,
            Some((4, 5)),
            Some("python:module:types"),
        );
        insert_relation_edge_row(
            &conn,
            "inherits_from",
            "python:class:types.Other",
            "python:class:types.Base",
            "resolved",
            None,
            "core:file:types.py",
            34,
            38,
        );
    }
    let resp = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:class:types.Base", "limit": 1}),
    )
    .await;
    assert_eq!(
        resp["result"]["relations_in"].as_array().unwrap().len(),
        1,
        "{resp}"
    );
    assert_eq!(resp["result"]["truncated"]["relations_in"], true, "{resp}");
}

#[tokio::test]
async fn orientation_pack_includes_relation_neighbors() {
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "orientation_pack",
        json!({"entity": "python:class:types.Base"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    let neighbors = &resp["result"]["neighbors"];
    let relations_in = neighbors["relations_in"].as_array().unwrap();
    assert_eq!(relations_in.len(), 1, "{resp}");
    assert_eq!(relations_in[0]["kind"], "inherits_from");
    assert_eq!(relations_in[0]["entity"]["id"], "python:class:types.Child");
    assert!(
        neighbors["relations_out"].as_array().unwrap().is_empty(),
        "{resp}"
    );
    // The omitted block reports the relation buckets alongside the others.
    assert_eq!(resp["result"]["omitted"]["relations_in"], 0, "{resp}");
    assert_eq!(resp["result"]["omitted"]["relations_out"], 0, "{resp}");
}

#[tokio::test]
async fn relation_list_candidates_keep_blocked_drop_phantom() {
    // An ambiguous edge's `candidates` carry raw entity ids. Under A3
    // (clarion-719e7320f5) a briefing-blocked candidate that resolves to a real
    // entity row keeps its navigable id and may appear, but a phantom candidate
    // (`types.wrap_again` — no entity row) must still be filtered out: only ids
    // resolvable to real entities pass.
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    {
        let conn = Connection::open(&db_path).expect("open sqlite");
        let app_path = project.path().join("app.py");
        insert_entity(
            &conn,
            "python:function:app.other",
            "function",
            &app_path,
            Some((2, 3)),
            Some("python:module:app"),
        );
        insert_relation_edge_row(
            &conn,
            "decorates",
            "python:function:types.wrap",
            "python:function:app.other",
            "ambiguous",
            Some(json!({"candidates": [
                "python:function:types.wrap",
                "python:function:types.wrap_again"
            ]})),
            "core:file:app.py",
            1,
            5,
        );
    }
    // Block the chosen from-side decorator; the alternative candidate id
    // (wrap_again) has no entity row at all and must also not be disclosed
    // as a visible "alternative" — only ids resolvable to VISIBLE entities
    // may pass through.
    mark_blocked(&db_path, "python:function:types.wrap", "secret_present");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:function:app.other", "direction": "in", "confidence": "ambiguous"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    let relations = resp["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{resp}");
    // The blocked from-side neighbor keeps its navigable identity (A3)…
    assert_blocked_identity_present(&relations[0]["entity"], "secret_present");
    assert_eq!(
        relations[0]["entity"]["id"], "python:function:types.wrap",
        "{resp}"
    );
    // …and `types.wrap` survives as a real candidate, but the phantom
    // `types.wrap_again` (no entity row) is filtered out.
    let candidates = relations[0]["candidates"].as_array().unwrap();
    assert!(
        candidates.iter().any(|c| c == "python:function:types.wrap"),
        "real blocked candidate should pass: {resp}"
    );
    assert!(
        !resp.to_string().contains("types.wrap_again"),
        "phantom candidate (no entity row) must not be disclosed: {resp}"
    );
}

#[tokio::test]
async fn relation_list_real_candidates_survive_blocked_sibling() {
    // Under A3 a blocked candidate that resolves to a real entity row keeps its
    // navigable id, so with one blocked and one visible candidate BOTH pass —
    // the only thing the filter still drops is a phantom (no-row) candidate.
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    {
        let conn = Connection::open(&db_path).expect("open sqlite");
        let types_path = project.path().join("types.py");
        let app_path = project.path().join("app.py");
        insert_entity(
            &conn,
            "python:function:types.wrap_again",
            "function",
            &types_path,
            Some((7, 8)),
            Some("python:module:types"),
        );
        insert_entity(
            &conn,
            "python:function:app.other",
            "function",
            &app_path,
            Some((2, 3)),
            Some("python:module:app"),
        );
        insert_relation_edge_row(
            &conn,
            "decorates",
            "python:function:types.wrap",
            "python:function:app.other",
            "ambiguous",
            Some(json!({"candidates": [
                "python:function:types.wrap",
                "python:function:types.wrap_again"
            ]})),
            "core:file:app.py",
            1,
            5,
        );
    }
    mark_blocked(
        &db_path,
        "python:function:types.wrap_again",
        "secret_present",
    );
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:function:app.other", "direction": "in", "confidence": "ambiguous"}),
    )
    .await;
    let relations = resp["result"]["relations"].as_array().unwrap();
    assert_eq!(relations.len(), 1, "{resp}");
    // Both candidates resolve to real entity rows, so both pass (A3); the
    // blocked sibling is navigable, not withheld.
    assert_eq!(
        relations[0]["candidates"],
        json!([
            "python:function:types.wrap",
            "python:function:types.wrap_again"
        ]),
        "{resp}"
    );
}

#[tokio::test]
async fn relation_list_redacts_line_text_for_drifted_anchor_file() {
    // The anchor owner of a relation edge is the edge's core file row; when
    // the on-disk file no longer matches that row's indexed content_hash, the
    // tool must NOT serve newly modified, scanner-unvetted bytes (same guard
    // call_sites pins for call anchors).
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    std::fs::write(
        project.path().join("types.py"),
        "API_KEY = \"sk-sentinel-never-disclose\"\n\nclass Base:\n    pass\n\nclass Child(Base):\n    pass\n",
    )
    .expect("rewrite types source");
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base", "direction": "in"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    let rel = &resp["result"]["relations"].as_array().unwrap()[0];
    assert_eq!(rel["source_status"], "drifted", "{rel}");
    assert!(rel["line"].is_null(), "{rel}");
    assert_eq!(rel["line_text"], "", "{rel}");
    assert!(
        rel["drift"]["stored_content_hash"].is_string()
            && rel["drift"]["current_content_hash"].is_string(),
        "{rel}"
    );
    assert!(
        !resp.to_string().contains("sk-sentinel-never-disclose"),
        "drifted anchor served unvetted bytes: {resp}"
    );
}

#[tokio::test]
async fn neighborhood_relation_buckets_gate_ambiguous_by_confidence() {
    // The relations buckets honor the tool's confidence tier (the tool
    // description promises "ambiguous and inferred ... are opt-in"); a
    // regression in relation_neighbors' gate would mix ambiguous relation
    // edges into default-resolved views.
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    {
        let conn = Connection::open(&db_path).expect("open sqlite");
        let app_path = project.path().join("app.py");
        insert_entity(
            &conn,
            "python:function:app.other",
            "function",
            &app_path,
            Some((2, 3)),
            Some("python:module:app"),
        );
        insert_relation_edge_row(
            &conn,
            "decorates",
            "python:function:types.wrap",
            "python:function:app.other",
            "ambiguous",
            None,
            "core:file:app.py",
            1,
            5,
        );
    }
    let state = state_for(project.path(), &db_path);

    let resolved = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:app.other"}),
    )
    .await;
    assert!(
        resolved["result"]["relations_in"]
            .as_array()
            .unwrap()
            .is_empty(),
        "default resolved tier must exclude the ambiguous relation edge: {resolved}"
    );

    let ambiguous = call_tool(
        &state,
        "neighborhood",
        json!({"id": "python:function:app.other", "confidence": "ambiguous"}),
    )
    .await;
    let rel_in = ambiguous["result"]["relations_in"].as_array().unwrap();
    assert_eq!(rel_in.len(), 1, "{ambiguous}");
    assert_eq!(rel_in[0]["edge_confidence"], "ambiguous", "{ambiguous}");
}

#[tokio::test]
async fn relation_list_survives_null_anchor_file_dangling_neighbor_and_overrun_cursor() {
    let (project, db_path) = open_project();
    seed_relation_fixture(project.path(), &db_path);
    {
        let conn = Connection::open(&db_path).expect("open sqlite");
        let types_path = project.path().join("types.py");
        insert_entity(
            &conn,
            "python:class:types.Anchorless",
            "class",
            &types_path,
            Some((4, 5)),
            Some("python:module:types"),
        );
        // A relation edge with NO source_file_id (a pre-anchored-file row, or
        // a partially failed analyze): evidence degrades to "unavailable",
        // never a panic.
        conn.execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence, source_byte_start, source_byte_end)
             VALUES ('inherits_from', 'python:class:types.Anchorless', 'python:class:types.Base', 'resolved', 60, 64)",
            [],
        )
        .expect("insert anchorless relation edge");
        // A dangling edge whose from-side entity row is gone: skipped, not
        // served and not panicking. FKs forbid inserting it directly, so
        // insert legally and delete the entity afterwards (foreign_keys
        // defaults OFF on this raw test connection) — simulating the reader
        // skew / corruption the handler's skip branch guards against.
        insert_entity(
            &conn,
            "python:class:types.Vanished",
            "class",
            &types_path,
            Some((4, 5)),
            Some("python:module:types"),
        );
        conn.execute(
            "INSERT INTO edges (kind, from_id, to_id, confidence, source_byte_start, source_byte_end)
             VALUES ('inherits_from', 'python:class:types.Vanished', 'python:class:types.Base', 'resolved', 70, 74)",
            [],
        )
        .expect("insert soon-dangling relation edge");
        conn.execute(
            "DELETE FROM entities WHERE id = 'python:class:types.Vanished'",
            [],
        )
        .expect("orphan the relation edge");
    }
    let state = state_for(project.path(), &db_path);

    let resp = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base", "direction": "in"}),
    )
    .await;
    assert_eq!(resp["ok"], true, "{resp}");
    let relations = resp["result"]["relations"].as_array().unwrap();
    // Child (anchored) + Anchorless (no file) survive; Vanished is skipped.
    let ids: Vec<&str> = relations
        .iter()
        .map(|r| r["entity"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["python:class:types.Anchorless", "python:class:types.Child"],
        "{resp}"
    );
    let anchorless = &relations[0];
    assert_eq!(anchorless["source_status"], "unavailable", "{anchorless}");
    assert!(anchorless["line"].is_null(), "{anchorless}");
    assert!(anchorless["file"].is_null(), "{anchorless}");

    // A cursor past the end yields an empty page, not an error.
    let past_end = call_tool(
        &state,
        "entity_relation_list",
        json!({"id": "python:class:types.Base", "direction": "in", "cursor": "50"}),
    )
    .await;
    assert_eq!(past_end["ok"], true, "{past_end}");
    assert!(
        past_end["result"]["relations"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{past_end}"
    );
    assert_eq!(past_end["result"]["truncated"], false, "{past_end}");
    assert_eq!(past_end["result"]["next_cursor"], Value::Null, "{past_end}");
}

// ── C-12 (weft-4165f1ed71): one freshness oracle, both surfaces agree ─────────

/// Set a file's or directory's mtime deterministically.
fn set_path_mtime(path: &std::path::Path, when: std::time::SystemTime) {
    std::fs::File::options()
        .read(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

/// Reproduces the dogfood-4 B1 divergence (weft-4165f1ed71): at the same
/// instant, `project_status_get` said `staleness: "stale"` while
/// `index_diff_get` said `overall: "fresh"` on the SAME store. Driver: the
/// status surface ran its own mtime/structural detector, which (a) watched the
/// PARENT of every ingested path — and the lacuna project-anchor entity
/// carries `source_file_path = <project root>`, putting the project root's
/// parent (`/home/john`, churning constantly) in the watch set — and (b)
/// treated a directory mtime as a file-modification signal. Convention C-12:
/// each status question gets exactly ONE authoritative verdict surface
/// (`index_diff_get`), and `project_status_get` must derive from the same code
/// path. Both surfaces must answer "fresh" here.
#[tokio::test]
async fn project_status_and_index_diff_share_one_freshness_verdict() {
    let outer = tempfile::tempdir().expect("outer dir");
    let root = outer.path().join("proj");
    let loomweave_dir = root.join(".weft/loomweave");
    std::fs::create_dir_all(&loomweave_dir).expect("create store dir");
    let db_path = loomweave_dir.join("loomweave.db");
    let mut conn = Connection::open(&db_path).expect("open sqlite");
    loomweave_storage::pragma::apply_write_pragmas(&conn).expect("pragmas");
    loomweave_storage::schema::apply_migrations(&mut conn).expect("migrations");

    // One real ingested source file, untouched since the analyze.
    let source = root.join("demo.py");
    std::fs::write(&source, "x = 1\n").expect("write source");
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_path, \
            properties, created_at, updated_at) \
         VALUES ('python:module:demo', 'python', 'module', 'demo', 'demo', ?1, '{}', \
                 '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        params![source.to_str().unwrap()],
    )
    .expect("insert module entity");
    // The lacuna shape: a synthetic project anchor whose source_file_path is
    // the PROJECT ROOT DIRECTORY itself.
    conn.execute(
        "INSERT INTO entities (id, plugin_id, kind, name, short_name, source_file_path, \
            properties, created_at, updated_at) \
         VALUES ('core:project:proj', 'core', 'project', 'proj', 'proj', ?1, \
                 '{\"finding_anchor\": true}', \
                 '2026-01-01T00:00:00.000Z', '2026-01-01T00:00:00.000Z')",
        params![root.to_str().unwrap()],
    )
    .expect("insert project anchor");
    // Analyze completed 2026-03-01; every ingested path is older than that…
    conn.execute(
        "INSERT INTO runs (id, started_at, completed_at, config, stats, status) \
         VALUES ('run-1', '2026-03-01T00:00:00.000Z', '2026-03-01T00:00:00.000Z', \
                 '{}', '{}', 'completed')",
        [],
    )
    .expect("insert run");
    drop(conn);

    // 2026-02-01 (before the run) for the source file and the project root dir…
    let before_run = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_769_904_000);
    set_path_mtime(&source, before_run);
    set_path_mtime(&root, before_run);
    // …while the PARENT of the project root churns after the run (the
    // /home/john situation). Outer tempdir mtime is "now" already; make it
    // explicit and unambiguous.
    set_path_mtime(outer.path(), std::time::SystemTime::now());

    let state = state_for(&root, &db_path);
    let status = call_tool(&state, "project_status", json!({})).await;
    let diff = call_tool(&state, "index_diff", json!({})).await;

    assert_eq!(status["ok"], true, "{status}");
    assert_eq!(diff["ok"], true, "{diff}");
    assert_eq!(
        diff["result"]["overall"], "fresh",
        "authoritative verdict: nothing indexed changed: {diff}"
    );
    assert_eq!(
        status["result"]["staleness"], "fresh",
        "project_status must derive from index_diff's verdict (C-12) — parent-dir \
         churn and the project anchor's directory path are not staleness: {status}"
    );
}

// ── B9 (weft-4a46553503): hydrated issue stub + honest enrichment degrade ────

/// B9 failing-first: a matched row's `issue` stub must carry the issue's `id`
/// alongside title/status — an agent acting on the row needs the id without
/// re-deriving it from the sibling `issue_id` field or making a second
/// filigree call.
#[tokio::test]
async fn issues_for_issue_stub_carries_id_title_and_status() {
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
            .with_detail("filigree-fresh", "Refresh tokens", "building", 1),
    );
    let state = state_for_filigree(project.path(), &db_path, client);

    let envelope = call_tool(&state, "issues_for", json!({"id": "python:module:demo"})).await;
    assert_eq!(envelope["ok"], true, "{envelope}");
    let issue = &envelope["result"]["matched"][0]["issue"];
    assert_eq!(
        issue["id"], "filigree-fresh",
        "stub must carry the id: {envelope}"
    );
    assert_eq!(issue["title"], "Refresh tokens", "{envelope}");
    assert_eq!(issue["status"], "building", "{envelope}");
}

/// B9 failing-first, degrade half: when the detail fetch fails (the dogfood-4
/// observation — every issue came back `issue: null` with no explanation
/// because the enrichment 401'd), the envelope must say so IN-BAND with a
/// top-level marker, not leave the consumer staring at inexplicable nulls.
#[tokio::test]
async fn issues_for_discloses_detail_enrichment_degrade_in_band() {
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
            .with_detail("filigree-fresh", "Refresh tokens", "building", 1)
            .with_detail_error(),
    );
    let state = state_for_filigree(project.path(), &db_path, client);

    let envelope = call_tool(&state, "issues_for", json!({"id": "python:module:demo"})).await;
    assert_eq!(envelope["ok"], true, "{envelope}");
    // Degraded, not failed: the association result still lands…
    assert_eq!(
        envelope["result"]["matched"][0]["issue_id"],
        "filigree-fresh"
    );
    assert_eq!(envelope["result"]["matched"][0]["issue"], Value::Null);
    // …and the degrade is disclosed once, in-band.
    let degraded = &envelope["result"]["issue_detail_unavailable"];
    assert!(
        degraded["reason"]
            .as_str()
            .is_some_and(|r| r.contains("503")),
        "the marker must carry the enrichment failure reason: {envelope}"
    );
}

/// The healthy path carries NO degrade marker (the marker must mean something).
#[tokio::test]
async fn issues_for_omits_degrade_marker_when_enrichment_succeeds() {
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
            .with_detail("filigree-fresh", "Refresh tokens", "building", 1),
    );
    let state = state_for_filigree(project.path(), &db_path, client);

    let envelope = call_tool(&state, "issues_for", json!({"id": "python:module:demo"})).await;
    assert_eq!(envelope["ok"], true, "{envelope}");
    assert!(
        envelope["result"].get("issue_detail_unavailable").is_none()
            || envelope["result"]["issue_detail_unavailable"].is_null(),
        "no degrade marker on the healthy path: {envelope}"
    );
}
