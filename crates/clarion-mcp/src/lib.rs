//! MCP protocol surface for Clarion.

mod analyze_runs;
pub mod config;
pub mod filigree;
pub mod filigree_url;
mod index_diff;
pub mod snapshot;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clarion_core::{
    EdgeConfidence, INFERRED_CALLS_PROMPT_VERSION, InferredCallsPromptInput,
    LEAF_SUMMARY_PROMPT_TEMPLATE_ID, LeafSummaryPromptInput, LlmProvider, LlmProviderError,
    LlmPurpose, LlmRequest, LlmResponse, build_inferred_calls_prompt, build_leaf_summary_prompt,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use time::{Date, Month, OffsetDateTime, macros::format_description};
use tokio::sync::{Mutex as AsyncMutex, broadcast, mpsc, oneshot};

use clarion_core::plugin::{ContentLengthCeiling, Frame, TransportError};
use clarion_storage::{
    CallEdgeMatch, EntityRow, InferredCallEdgeRecord, InferredEdgeCacheEntry, InferredEdgeCacheKey,
    InferredEdgeWriteStats, ReaderPool, ReferenceDirection, ReferenceEdgeMatch,
    RolledUpReferenceEdge, StorageError, SummaryCacheEntry, SummaryCacheKey, UnresolvedCallSiteRow,
    WriterCmd, ancestor_chain, call_edges_from, call_edges_targeting,
    candidate_entities_for_unresolved_sites, child_entity_ids, contained_entity_ids,
    entities_containing_line, entity_by_id, existing_entity_ids, find_entities,
    import_edges_for_entity, inferred_edge_cache_key_id, inferred_edge_cache_lookup,
    module_reference_rollup, normalize_source_path, reference_edges_for_entity, subsystem_members,
    subsystem_of_entity, summary_cache_lookup, unresolved_call_sites_for_caller,
    unresolved_callers_for_target,
};

use crate::config::LlmConfig;
use crate::filigree::{EntityAssociation, EntityAssociationsResponse, FiligreeLookup};

/// MCP protocol revision supported by the B.6 stdio server.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const EMPTY_GUIDANCE_FINGERPRINT: &str = "guidance-empty";

/// The bundled clarion-workflow skill text, embedded for the `prompts/get`
/// surface and reused as the canonical orientation reference. Same file the
/// CLI installs on disk.
pub const CLARION_WORKFLOW_SKILL: &str =
    include_str!("../../clarion-cli/assets/skills/clarion-workflow/SKILL.md");

/// Orientation text returned in the MCP `initialize` result's `instructions`
/// field. The `Tools:` enumeration is derived from [`list_tools`] (the single
/// source of truth) so it can never drift from the advertised tool set as tools
/// are added or removed; the surrounding prose is static. Kept consistent with
/// the clarion-workflow skill.
fn server_instructions() -> String {
    let tool_names = list_tools()
        .iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Clarion is a code-archaeology server: it has pre-extracted this project \
into a queryable map of entities (functions, classes, modules, files), the call \
/ reference / import edges between them, and subsystem clusters. Ask Clarion \
instead of re-reading or grepping the tree.

Entity IDs are `{{plugin}}:{{kind}}:{{qualified_name}}` (e.g. \
`python:function:pkg.mod.func`); subsystems are `core:subsystem:{{hash}}`. You \
almost never type IDs — get one from `find_entity` or `entity_at`, then copy it \
verbatim into the next tool.

Tools: {tool_names}. `callers_of` / `neighborhood` / `execution_paths_from` \
take a `confidence` tier (resolved | ambiguous | inferred; default resolved). \
`project_status` reports index freshness, counts, LLM policy, and the resolved \
Filigree endpoint.

For the full workflow see the clarion-workflow skill (installed by \
`clarion install --skills`), or read the `clarion-workflow` prompt. Live \
project counts and index freshness are in the `clarion://context` resource."
    )
}

type InferredInflight =
    Arc<AsyncMutex<HashMap<InferredEdgeCacheKey, broadcast::Sender<InferredDispatchOutcome>>>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[must_use]
// A flat registry of tool definitions; length tracks the tool count by design.
#[allow(clippy::too_many_lines)]
pub fn list_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "entity_at",
            description: "Return the innermost Clarion entity whose source range contains a file and line, plus an `entity_context` evidence block: match_reason (decorator_range / declaration / body_range / containing_range / no_match) explaining why the line matched, the module→entity containing stack, the matched entity's decl/body/decorator sub-ranges, any same-granularity ambiguity alternatives, and index freshness. Paths are normalized relative to the project root. A blank or comment line that only a module spans reports containing_range — never a fabricated exact match.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file": {"type": "string"},
                    "line": {"type": "integer", "minimum": 1}
                },
                "required": ["file", "line"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "find_entity",
            description: "Search Clarion entities by id, name, short name, and summary text stored on entity rows. Results are paginated and ranked by FTS match where possible. This does not traverse the graph and does not search on-demand summary_cache entries. Pass an optional `kind` (e.g. \"subsystem\", \"function\", \"class\", \"module\") to return only entities of that kind — the way to locate a subsystem without visually filtering results.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "minLength": 1},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                    "cursor": {"type": ["string", "null"]},
                    "kind": {"type": "string", "minLength": 1}
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "callers_of",
            description: "Return entities that call the given entity. Default confidence is resolved, so ambiguous static candidates and LLM-inferred edges are excluded unless explicitly requested. Ambiguous edges expand all candidates; inferred edges may trigger bounded LLM dispatch. The result carries scope_excludes naming static blind spots not searched (e.g. attribute-receiver-calls) so an empty callers list is never read as a guaranteed true negative.",
            input_schema: id_confidence_schema(),
        },
        ToolDefinition {
            name: "execution_paths_from",
            description: "Return bounded calls-only execution paths starting at an entity. Default confidence is resolved. max_depth defaults to 3. Results are compact: a deduplicated nodes table plus paths as arrays of node ids (under a root), ranked longest-first. Traversal stops at the server edge cap and the response is capped at a maximum number of ranked paths; truncated/truncation_reason report edge-cap or path-cap when either trims. The result carries scope_excludes naming static blind spots not searched (e.g. attribute-receiver-calls).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "max_depth": {"type": "integer", "minimum": 1, "maximum": 8},
                    "confidence": confidence_schema()
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "summary",
            description: "Return an on-demand cached summary for one entity. In v0.1 this is leaf scope only: module summaries describe the module docstring and top-level members, not an aggregation of contained function/class summaries. If the LLM returns non-JSON the response degrades to a deterministic structural summary (kind: structural-fallback) built from the entity source, and that fallback is cached so a retry is a free cache hit rather than a re-billed failure.",
            input_schema: id_schema(),
        },
        ToolDefinition {
            name: "issues_for",
            description: "Return Filigree issues attached to this Clarion entity, optionally including issues attached to contained entities. Filigree is an enrichment source; if unavailable, the tool returns an unavailable envelope instead of failing Clarion. The result carries a result_kind (matched | no_matches | unavailable) so a reachable-but-empty Filigree is distinct from an unreachable one, and a filigree_endpoint block (configured vs resolved URL + resolution_source) so you can see which endpoint — e.g. a live ethereal port — the answer came from.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "include_contained": {"type": "boolean"}
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "neighborhood",
            description: "Return the one-hop Clarion neighborhood around an entity: callers, callees, container, contained entities, references, and imports (imports_in = who imports this module, imports_out = what it imports; module-to-module). Default confidence is resolved; ambiguous and inferred calls are opt-in. References and imports are not execution flow. When the entity is a module, references_in/references_out are rolled up over the symbols it contains (references_rolled_up=true) — each neighbor carries a `via` naming the contained symbol the edge touches, so \"who imports this module/contract\" is answered at module altitude rather than reading empty. The result carries scope_excludes naming blind spots not searched (e.g. attribute-receiver-calls) so empty sections are never read as guaranteed true negatives.",
            input_schema: id_confidence_schema(),
        },
        ToolDefinition {
            name: "subsystem_members",
            description: "List module entities assigned to a subsystem entity.",
            input_schema: id_schema(),
        },
        ToolDefinition {
            name: "subsystem_of",
            description: "Return the subsystem an entity belongs to — the reverse of subsystem_members. Accepts any entity id: a module resolves directly, while a function/class resolves through its nearest containing module. Returns the subsystem id/name and the module the membership was resolved through, or a no-subsystem result when the entity has no subsystem-assigned module ancestor.",
            input_schema: id_schema(),
        },
        ToolDefinition {
            name: "project_status",
            description: "Return deterministic Clarion diagnostics: repo root, db path, latest run (id/status/started/completed), entity/subsystem/edge/finding/briefing-blocked counts, index staleness, per-plugin entity counts from the current index, LLM policy (provider/live/cache), and the resolved Filigree endpoint (configured vs resolved URL + resolution source). Answers \"is the graph fresh, plugin-less, LLM-live, Filigree-reachable?\" without shelling out. No LLM call.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "summary_preview_cost",
            description: "Preview what calling summary(id) would cost BEFORE spending. Reports cache_status (hit | expired | miss), the cached row's real tokens/cost/age on a hit, an input-token estimate on a miss, the configured model, the LLM policy (provider/live/allow_live_provider/cache horizon), and live_spend_would_occur — true only when no fresh cache row exists AND a live provider is wired. A disabled/unconfigured LLM is reported distinctly from a cache miss. Never invokes the LLM provider.",
            input_schema: id_schema(),
        },
        ToolDefinition {
            name: "source_for_entity",
            description: "Return the exact indexed source span for one entity (its source_line_start..source_line_end, which includes any decorators/signature/docstring the plugin captured) plus a bounded window of surrounding context, as line-numbered lines each flagged in_entity true/false. No LLM call. Lets an agent read and trust the entity without shelling out. source_status reports `ok`, or — instead of a misleading stale snippet — `missing` (file gone), `no_range`/`no_source_path` (entity has no anchor), `binary` (non-UTF-8), or `drifted` (the file no longer matches the indexed content_hash; rerun `clarion analyze`). context_lines defaults to 10.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "context_lines": {"type": "integer", "minimum": 0, "maximum": 200}
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "call_sites",
            description: "Show the actual source sites behind calls/references edges, so an agent can see WHY Clarion believes an edge exists rather than trusting it blind. role=caller (default) returns this entity's outgoing sites (what it calls/references); role=callee returns incoming sites (who calls/references it). Each site carries the file path, 1-based line, byte column, the source line text, edge kind, confidence, and a resolution of resolved | ambiguous (with candidate ids) | unresolved (a static call Clarion could not bind, kept separate so it is never mixed with resolved evidence). Filter by edge kind (`calls`/`references`) and by a best-effort production/test path heuristic (`all`/`production`/`test`; path partitioning is not indexed — the heuristic matches conventional test paths). Output is bounded; truncated flags when the site cap trims. No LLM call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "role": {"type": "string", "enum": ["caller", "callee"], "default": "caller"},
                    "kind": {"type": "string", "enum": ["calls", "references"]},
                    "confidence": confidence_schema(),
                    "path": {"type": "string", "enum": ["all", "production", "test"], "default": "all"}
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "orientation_pack",
            description: "Assemble one deterministic orientation packet for a code location — the replacement for hand-composing find_entity + entity_at + source reads + neighborhood + issues_for + freshness on every question. Resolve EITHER by `entity` id OR by `file`+`line` (exactly one form). The packet bundles: the primary entity, the entity_context evidence (match_reason / containing stack / decl-body-decorator ranges — so a decorator-line query is explained, not guessed), a compact source-span summary, one-hop neighbors (callers, callees, container, contained, references, imports — for a module, references_in/out are rolled up over contained symbols with references_rolled_up=true), compact resolved execution paths, related Filigree issues, index/Filigree/LLM health, warnings, and suggested next reads. No LLM summary is invoked. Every list is bounded; an `omitted` block reports per-section truncation counts and `degraded` sections name surfaces that were unavailable (e.g. Filigree down) so an empty section is never read as a guaranteed negative.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "entity": {"type": "string", "minLength": 1},
                    "file": {"type": "string", "minLength": 1},
                    "line": {"type": "integer", "minimum": 1}
                },
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "analyze_start",
            description: "Start a `clarion analyze` run over this project in the background and return its run handle immediately — do not block on the (possibly many-minute) run. Re-indexes the source tree and refreshes entities/edges/subsystems. Returns run_id, status (`started`), and the progress-file path. Only one analyze may run per project at a time (a cross-process lock enforces it); a second start while one is active is rejected. Poll analyze_status for progress; analyze_cancel to stop. No arguments.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "analyze_status",
            description: "Report the live status of an analyze run started via analyze_start. status is one of queued (spawned, not yet recording) | running | completed | failed | cancelled | skipped_no_plugins. While running it exposes phase (discovering / analyzing / clustering), current_plugin, processed_files / total_files, current_file, the latest heartbeat_at, elapsed_seconds, and progress_observed (false when the heartbeat has gone stale — the run may be wedged). On a terminal status it carries the recorded run stats. Reads structured progress, never logs.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "run_id": {"type": "string", "minLength": 1}
                },
                "required": ["run_id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "analyze_cancel",
            description: "Cancel a running analyze. SIGKILLs the run's whole process group — terminating the language plugin and its pyright-langserver child — then marks the run terminal (status `cancelled`) so it is never left dangling as `running`. Idempotent: cancelling an already-terminal run reports its current state. Partial work already written is kept (cancel discards in-flight work, not the index).",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "run_id": {"type": "string", "minLength": 1}
                },
                "required": ["run_id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "index_diff",
            description: "Report what changed since the last analyze and whether this checkout is newer than the graph — so an agent need not hand-roll git + mtime freshness checks. Compares: analyzed_at (last completed run) vs current git HEAD (with head_newer_than_analyze derived from HEAD's committer date vs run completion, true even when source mtimes are ambiguous); indexed source files modified or now-missing since analyze; dirty working-tree files flagged when they touch an indexed path; and per-run aggregate plugin skip/drop counters. Git is read at query time, read-only, and fail-soft: a missing git binary or non-repo dir degrades to git.available=false with a reason rather than failing. analyzed_commit is null by design (Clarion persists no analyze-time SHA). overall is fresh | drift | unknown | never_analyzed; lists are bounded with an `omitted` block. entity-level add/remove/change diff is unavailable in v0.1 (only the current graph is retained). No LLM call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "maximum": 2000}
                },
                "additionalProperties": false
            }),
        },
    ]
}

fn confidence_schema() -> Value {
    json!({
        "type": "string",
        "enum": ["resolved", "ambiguous", "inferred"],
        "default": "resolved"
    })
}

fn id_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "minLength": 1}
        },
        "required": ["id"],
        "additionalProperties": false
    })
}

fn id_confidence_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": {"type": "string", "minLength": 1},
            "confidence": confidence_schema()
        },
        "required": ["id"],
        "additionalProperties": false
    })
}

/// Handle state-free MCP requests such as `initialize` and `tools/list`.
///
/// Storage-backed tool calls require [`ServerState::handle_json_rpc`]. The
/// `resources/*` and `prompts/*` RPCs are likewise served ONLY by
/// [`ServerState::handle_json_rpc`] (the production `clarion serve` path); a
/// caller using this free function gets a deliberately narrower server, and its
/// `initialize` advertises only the `tools` capability it actually serves.
#[must_use]
pub fn handle_json_rpc(request: &Value) -> Option<Value> {
    if is_json_rpc_notification(request) {
        return None;
    }
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return Some(error_response(&id, -32600, "invalid request"));
    };

    Some(match method {
        "initialize" => result_response(&id, &initialize_result(false)),
        "tools/list" => result_response(&id, &json!({"tools": list_tools()})),
        "tools/call" => error_response(
            &id,
            -32601,
            "tools/call requires ServerState::handle_json_rpc",
        ),
        _ => error_response(&id, -32601, "method not found"),
    })
}

/// Deterministic, non-storage diagnostics threaded in at server construction so
/// `project_status` can report the LLM policy and the resolved Filigree
/// endpoint without re-reading config or re-running URL resolution. Optional:
/// servers built via [`ServerState::new`] (e.g. storage-only tests) omit it and
/// `project_status` reports those blocks as unconfigured.
#[derive(Debug, Clone)]
pub struct DiagnosticsContext {
    pub llm: LlmDiagnostics,
    pub filigree: crate::filigree_url::FiligreeUrlResolution,
}

/// The LLM policy posture, captured at construction. `live` reflects whether a
/// provider is actually wired (vs. merely permitted by config).
#[derive(Debug, Clone)]
pub struct LlmDiagnostics {
    /// Provider label, e.g. `"openrouter"`, `"codex_cli"`, `"recording"`, or
    /// `"disabled"` when no provider is wired.
    pub provider: String,
    /// A live provider is wired and summaries will dispatch to it.
    pub live: bool,
    /// Whether config permits a live provider at all (`llm.allow_live_provider`).
    pub allow_live_provider: bool,
    /// Summary-cache freshness horizon in days (`llm.cache_max_age_days`).
    pub cache_max_age_days: u32,
}

pub struct ServerState {
    project_root: PathBuf,
    readers: ReaderPool,
    execution_edge_cap: usize,
    execution_path_cap: usize,
    summary_llm: Option<SummaryLlmState>,
    clock: Arc<dyn Fn() -> String + Send + Sync>,
    budget: Arc<Mutex<BudgetLedger>>,
    inferred_inflight: InferredInflight,
    filigree_client: Option<Arc<dyn FiligreeLookup>>,
    diagnostics: Option<DiagnosticsContext>,
    /// Supervised `clarion analyze` runs launched via `analyze_start`.
    analyze_runs: crate::analyze_runs::RunRegistry,
    /// Launcher for `analyze_start` to spawn. `None` → `current_exe()`; tests
    /// inject a stub via [`ServerState::with_analyze_command`].
    analyze_program: Option<PathBuf>,
}

impl ServerState {
    #[must_use]
    pub fn new(project_root: PathBuf, readers: ReaderPool) -> Self {
        Self {
            project_root,
            readers,
            execution_edge_cap: 500,
            execution_path_cap: 200,
            summary_llm: None,
            clock: Arc::new(default_now_string),
            budget: Arc::new(Mutex::new(BudgetLedger::default())),
            inferred_inflight: Arc::new(AsyncMutex::new(HashMap::new())),
            filigree_client: None,
            diagnostics: None,
            analyze_runs: Arc::new(Mutex::new(HashMap::new())),
            analyze_program: None,
        }
    }

    /// Override the program `analyze_start` launches (default: `current_exe()`).
    /// Tests inject a stub binary so the lifecycle can be exercised without a
    /// full analyze run.
    #[must_use]
    pub fn with_analyze_command(mut self, program: PathBuf) -> Self {
        self.analyze_program = Some(program);
        self
    }

    #[must_use]
    pub fn with_edge_cap(mut self, execution_edge_cap: usize) -> Self {
        self.execution_edge_cap = execution_edge_cap;
        self
    }

    #[must_use]
    pub fn with_path_cap(mut self, execution_path_cap: usize) -> Self {
        self.execution_path_cap = execution_path_cap;
        self
    }

    #[must_use]
    pub fn with_summary_llm(
        mut self,
        writer: mpsc::Sender<WriterCmd>,
        config: LlmConfig,
        provider: Arc<dyn LlmProvider>,
    ) -> Self {
        self.summary_llm = Some(SummaryLlmState {
            writer,
            config,
            provider,
        });
        self
    }

    #[must_use]
    pub fn with_clock(mut self, clock: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    #[must_use]
    pub fn with_filigree_client(mut self, client: Arc<dyn FiligreeLookup>) -> Self {
        self.filigree_client = Some(client);
        self
    }

    #[must_use]
    pub fn with_diagnostics(mut self, diagnostics: DiagnosticsContext) -> Self {
        self.diagnostics = Some(diagnostics);
        self
    }

    pub async fn handle_json_rpc(&self, request: &Value) -> Option<Value> {
        if is_json_rpc_notification(request) {
            return None;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            return Some(error_response(&id, -32600, "invalid request"));
        };

        Some(match method {
            "initialize" => result_response(&id, &initialize_result(true)),
            "tools/list" => result_response(&id, &json!({"tools": list_tools()})),
            "tools/call" => self.handle_tool_call(&id, request.get("params")).await,
            "resources/list" => result_response(&id, &resources_list()),
            "resources/read" => self.handle_resources_read(&id, request.get("params")).await,
            "prompts/list" => result_response(&id, &prompts_list()),
            "prompts/get" => prompts_get(&id, request.get("params")),
            _ => error_response(&id, -32601, "method not found"),
        })
    }

    async fn handle_tool_call(&self, id: &Value, params: Option<&Value>) -> Value {
        let Some(params) = params.and_then(Value::as_object) else {
            return error_response(id, -32602, "invalid tools/call params");
        };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error_response(id, -32602, "invalid tools/call params: missing name");
        };
        if !list_tools().iter().any(|tool| tool.name == name) {
            return error_response(id, -32601, &format!("unknown tool: {name}"));
        }
        let arguments = params.get("arguments").unwrap_or(&Value::Null);
        let Some(arguments) = arguments.as_object() else {
            return error_response(
                id,
                -32602,
                "invalid tools/call params: arguments must be object",
            );
        };

        let envelope = match name {
            "entity_at" => match self.tool_entity_at(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "find_entity" => match self.tool_find_entity(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "callers_of" => match self.tool_callers_of(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "execution_paths_from" => match self.tool_execution_paths_from(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "neighborhood" => match self.tool_neighborhood(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "summary" => match self.tool_summary(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "issues_for" => match self.tool_issues_for(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "subsystem_members" => match self.tool_subsystem_members(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "subsystem_of" => match self.tool_subsystem_of(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "project_status" => match self.tool_project_status(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "summary_preview_cost" => match self.tool_summary_preview_cost(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "source_for_entity" => match self.tool_source_for_entity(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "call_sites" => match self.tool_call_sites(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "orientation_pack" => match self.tool_orientation_pack(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "analyze_start" => match self.tool_analyze_start(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "analyze_status" => match self.tool_analyze_status(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "analyze_cancel" => match self.tool_analyze_cancel(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "index_diff" => match self.tool_index_diff(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            _ => unreachable!("known tools checked above"),
        };

        tool_json_rpc_response(id, &envelope)
    }

    async fn handle_resources_read(&self, id: &Value, params: Option<&Value>) -> Value {
        let Some(uri) = params
            .and_then(Value::as_object)
            .and_then(|p| p.get("uri"))
            .and_then(Value::as_str)
        else {
            return error_response(id, -32602, "invalid resources/read params: missing uri");
        };
        if uri != "clarion://context" {
            return error_response(id, -32602, &format!("unknown resource: {uri}"));
        }
        let snapshot_json = self.context_snapshot_json().await;
        result_response(
            id,
            &json!({
                "contents": [
                    {
                        "uri": "clarion://context",
                        "mimeType": "application/json",
                        "text": snapshot_json
                    }
                ]
            }),
        )
    }

    async fn context_snapshot_json(&self) -> String {
        // Single fallback used by both the reader-error and serialize-error
        // branches: serialize a real `ProjectSnapshot` so the shape stays in
        // lock-step with the type as it gains fields. `degraded: true` — this
        // path is only reached when the reader pool errored or a healthy
        // snapshot failed to serialize, so the consumer must not read the zero
        // counts as a genuinely empty index.
        let fallback = || {
            let snap = crate::snapshot::unreadable_db_snapshot();
            serde_json::to_string(&snap).unwrap_or_default()
        };

        let project_root = self.project_root.clone();
        let snapshot = self
            .readers
            .with_reader(move |conn| Ok(crate::snapshot::project_snapshot(conn, &project_root)))
            .await;
        match snapshot {
            Ok(snap) => serde_json::to_string(&snap).unwrap_or_else(|_| fallback()),
            Err(err) => {
                tracing::warn!(error = %err, "clarion://context snapshot failed");
                fallback()
            }
        }
    }

    async fn tool_entity_at(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let file = required_str(arguments, "file")?.to_owned();
        let line = required_i64(arguments, "line")?;
        if line <= 0 {
            return Err(ParamError::new("line must be positive"));
        }
        let normalized = match normalize_source_path(&self.project_root, &file) {
            Ok(path) => path,
            Err(err) => {
                return Ok(tool_error_envelope("invalid-path", &err.to_string(), false));
            }
        };
        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                // Every entity whose span contains the line, innermost first
                // (same ordering as the legacy single-row `entity_at_line`).
                let candidates = entities_containing_line(conn, &normalized, line)?;
                let matched = candidates.first().cloned();
                let stack = match &matched {
                    Some(entity) => ancestor_chain(conn, &entity.id)?,
                    None => Vec::new(),
                };
                let snapshot = crate::snapshot::project_snapshot(conn, &project_root);
                Ok(json!({
                    "entity": matched.as_ref().map(entity_json),
                    "entity_context": entity_context_json(
                        Some(line),
                        matched.as_ref(),
                        &candidates,
                        &stack,
                        &snapshot,
                    ),
                }))
            })
            .await;
        Ok(envelope_from_storage_result(result))
    }

    async fn tool_find_entity(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let pattern = required_str(arguments, "pattern")?.to_owned();
        let limit = optional_usize(arguments, "limit")?
            .unwrap_or(20)
            .clamp(1, 100);
        let offset = match arguments.get("cursor") {
            None | Some(Value::Null) => 0,
            Some(Value::String(cursor)) => cursor
                .parse::<usize>()
                .map_err(|_| ParamError::new("cursor must be a numeric offset"))?,
            _ => return Err(ParamError::new("cursor must be a string or null")),
        };
        // Optional exact-match entity-kind filter (e.g. "subsystem"). Omitting it
        // preserves the unfiltered search. Validated as a non-blank string here;
        // unknown kinds simply match nothing (kinds are plugin-owned).
        let kind = match arguments.get("kind") {
            None | Some(Value::Null) => None,
            Some(Value::String(kind)) if !kind.trim().is_empty() => Some(kind.clone()),
            Some(Value::String(_)) => {
                return Err(ParamError::new("kind must be a non-empty string"));
            }
            _ => return Err(ParamError::new("kind must be a string or null")),
        };
        let result = self
            .readers
            .with_reader(move |conn| {
                let mut rows = find_entities(
                    conn,
                    &pattern,
                    limit.saturating_add(1),
                    offset,
                    kind.as_deref(),
                )?;
                let has_more = rows.len() > limit;
                rows.truncate(limit);
                let next_cursor = if has_more {
                    Some((offset + limit).to_string())
                } else {
                    None
                };
                Ok(json!({
                    "entities": rows.iter().map(entity_json).collect::<Vec<_>>(),
                    "next_cursor": next_cursor
                }))
            })
            .await;
        Ok(envelope_from_storage_result(result))
    }

    async fn tool_callers_of(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let confidence = optional_confidence(arguments)?;
        let stats_delta = if confidence == EdgeConfidence::Inferred {
            match self.ensure_inferred_for_target(&entity_id).await {
                Ok(stats) => stats.to_json(),
                Err(err) => return Ok(err.to_envelope()),
            }
        } else {
            json!({})
        };
        let result = self
            .readers
            .with_reader(move |conn| {
                if entity_by_id(conn, &entity_id)?.is_none() {
                    return Ok(tool_error_envelope(
                        "entity-not-found",
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                }
                let callers = call_edges_targeting(conn, &entity_id, confidence)?
                    .into_iter()
                    .filter_map(|edge| caller_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                Ok(success_envelope_with_stats(
                    json!({
                        "callers": callers,
                        "scope_excludes": call_graph_scope_excludes(confidence),
                    }),
                    stats_delta,
                ))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    async fn tool_execution_paths_from(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let max_depth = optional_usize(arguments, "max_depth")?
            .unwrap_or(3)
            .clamp(1, 8);
        let confidence = optional_confidence(arguments)?;
        if confidence == EdgeConfidence::Inferred {
            return Ok(self.inferred_execution_paths(entity_id, max_depth).await);
        }
        let edge_cap = self.execution_edge_cap;
        let path_cap = self.execution_path_cap;
        let result = self
            .readers
            .with_reader(move |conn| {
                if entity_by_id(conn, &entity_id)?.is_none() {
                    return Ok(tool_error_envelope(
                        "entity-not-found",
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                }
                let mut traversal = PathTraversal::new(edge_cap);
                let mut path = vec![entity_id.clone()];
                traversal.walk(conn, &entity_id, &mut path, max_depth, confidence)?;
                let edge_truncated = traversal.truncated;
                let edge_count_visited = traversal.edge_count_visited;
                let compact = compact_execution_paths(conn, traversal.paths, path_cap)?;
                Ok(success_envelope_with_truncation(
                    json!({
                        "root": entity_id,
                        "nodes": compact.nodes,
                        "paths": compact.paths,
                        "edge_count_visited": edge_count_visited,
                        "scope_excludes": call_graph_scope_excludes(confidence),
                    }),
                    path_truncation_reason(edge_truncated, compact.path_cap_truncated),
                ))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    async fn inferred_execution_paths(&self, entity_id: String, max_depth: usize) -> Value {
        let exists = self
            .readers
            .with_reader({
                let entity_id = entity_id.clone();
                move |conn| entity_by_id(conn, &entity_id).map(|entity| entity.is_some())
            })
            .await;
        match exists {
            Ok(true) => {}
            Ok(false) => {
                return tool_error_envelope(
                    "entity-not-found",
                    &format!("entity {entity_id} was not found"),
                    false,
                );
            }
            Err(err) => {
                return tool_error_envelope(
                    "storage-error",
                    &err.to_string(),
                    storage_retryable(&err),
                );
            }
        }

        let root = entity_id.clone();
        let mut stats = InferredDispatchStats::default();
        let mut dispatched_callers = BTreeSet::new();
        let mut stack = vec![(entity_id.clone(), vec![entity_id], max_depth)];
        let mut paths = Vec::new();
        let mut edge_count_visited = 0;
        let mut truncated = false;

        while let Some((current_id, path, remaining_depth)) = stack.pop() {
            if remaining_depth == 0 || truncated {
                continue;
            }
            if dispatched_callers.insert(current_id.clone()) {
                match self.ensure_inferred_for_caller(&current_id).await {
                    Ok(delta) => stats.merge(&delta),
                    Err(err) => return err.to_envelope(),
                }
            }
            let edges = match self
                .readers
                .with_reader({
                    let current_id = current_id.clone();
                    move |conn| call_edges_from(conn, &current_id, EdgeConfidence::Inferred)
                })
                .await
            {
                Ok(edges) => edges,
                Err(err) => {
                    return tool_error_envelope(
                        "storage-error",
                        &err.to_string(),
                        storage_retryable(&err),
                    );
                }
            };
            for edge in edges.into_iter().rev() {
                edge_count_visited += 1;
                if edge_count_visited > self.execution_edge_cap {
                    truncated = true;
                    break;
                }
                if path.iter().any(|seen| seen == &edge.to_id) {
                    continue;
                }
                let mut next_path = path.clone();
                next_path.push(edge.to_id.clone());
                paths.push(next_path.clone());
                stack.push((edge.to_id, next_path, remaining_depth - 1));
            }
        }

        let path_cap = self.execution_path_cap;
        let compacted = self
            .readers
            .with_reader(move |conn| compact_execution_paths(conn, paths, path_cap))
            .await;
        match compacted {
            Ok(compact) => success_envelope_with_truncation_and_stats(
                json!({
                    "root": root,
                    "nodes": compact.nodes,
                    "paths": compact.paths,
                    "edge_count_visited": edge_count_visited,
                    "scope_excludes": call_graph_scope_excludes(EdgeConfidence::Inferred),
                }),
                path_truncation_reason(truncated, compact.path_cap_truncated),
                stats.to_json(),
            ),
            Err(err) => {
                tool_error_envelope("storage-error", &err.to_string(), storage_retryable(&err))
            }
        }
    }

    async fn tool_neighborhood(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let confidence = optional_confidence(arguments)?;
        if confidence == EdgeConfidence::Inferred {
            if let Err(err) = self.ensure_inferred_for_target(&entity_id).await {
                return Ok(err.to_envelope());
            }
            if let Err(err) = self.ensure_inferred_for_caller(&entity_id).await {
                return Ok(err.to_envelope());
            }
        }
        let result = self
            .readers
            .with_reader(move |conn| {
                let Some(entity) = entity_by_id(conn, &entity_id)? else {
                    return Ok(tool_error_envelope(
                        "entity-not-found",
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                };
                let inbound_callers = call_edges_targeting(conn, &entity_id, confidence)?
                    .into_iter()
                    .filter_map(|edge| caller_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let outbound_calls = call_edges_from(conn, &entity_id, confidence)?
                    .into_iter()
                    .filter_map(|edge| callee_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let container_entity = entity
                    .parent_id
                    .as_deref()
                    .and_then(|parent_id| entity_by_id(conn, parent_id).transpose())
                    .transpose()?
                    .as_ref()
                    .map(entity_json);
                let contained_entities = child_entity_ids(conn, &entity_id)?
                    .iter()
                    .filter_map(|child_id| entity_by_id(conn, child_id).transpose())
                    .map(|row| row.map(|entity| entity_json(&entity)))
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let (references_in, references_rolled_up) = reference_neighbors_for(
                    conn,
                    &entity_id,
                    &entity.kind,
                    ReferenceDirection::In,
                )?;
                let (references_out, _) = reference_neighbors_for(
                    conn,
                    &entity_id,
                    &entity.kind,
                    ReferenceDirection::Out,
                )?;
                let imports_in = import_neighbors(conn, &entity_id, ReferenceDirection::In)?;
                let imports_out = import_neighbors(conn, &entity_id, ReferenceDirection::Out)?;
                let scope_excludes = call_graph_scope_excludes(confidence);
                Ok(success_envelope(json!({
                    "entity": entity_json(&entity),
                    "callers": inbound_callers,
                    "callees": outbound_calls,
                    "container": container_entity,
                    "contained": contained_entities,
                    "references_in": references_in,
                    "references_out": references_out,
                    // True when the entity is a module and references_in/out
                    // aggregate contained symbols' edges (each neighbor tagged
                    // with a `via` symbol); false for symbol-level entities
                    // whose references are direct (clarion-79d0ff6e14).
                    "references_rolled_up": references_rolled_up,
                    "imports_in": imports_in,
                    "imports_out": imports_out,
                    "scope_excludes": scope_excludes,
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    async fn tool_issues_for(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let include_contained = optional_bool(arguments, "include_contained")?.unwrap_or(true);
        // Surface the same configured-vs-resolved Filigree endpoint block that
        // `project_status` reports, so an agent can see WHICH endpoint a result
        // came from (e.g. an ethereal port resolved from
        // `.filigree/ephemeral.port`) instead of curling ports by hand. Null on
        // storage-only servers built without a diagnostics context.
        let endpoint = self.filigree_diagnostics_json();
        let Some(client) = self.filigree_client.clone() else {
            return Ok(issues_unavailable(
                &endpoint,
                "filigree-disabled",
                "Filigree integration is disabled",
            ));
        };
        let read = match self
            .read_issues_for_entities(entity_id, include_contained)
            .await
        {
            Ok(Some(read)) => read,
            Ok(None) => {
                return Ok(issues_unavailable(
                    &endpoint,
                    "entity-not-found",
                    "Clarion entity was not found",
                ));
            }
            Err(err) => {
                return Ok(tool_error_envelope(
                    "storage-error",
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };
        let mut accumulator = IssuesForAccumulator::new(&read.entities);
        let mut requests_total = 0_usize;
        for (idx, entity) in read.entities.iter().enumerate() {
            let entity_id = entity.id.clone();
            let client = client.clone();
            let response = match tokio::task::spawn_blocking(move || {
                client.associations_for(&entity_id)
            })
            .await
            {
                Ok(Ok(response)) => response,
                Ok(Err(err)) => {
                    return Ok(issues_unavailable(
                        &endpoint,
                        "filigree-unreachable",
                        &err.to_string(),
                    ));
                }
                Err(err) => {
                    return Ok(issues_unavailable(
                        &endpoint,
                        "filigree-client-error",
                        &format!("Filigree client task failed: {err}"),
                    ));
                }
            };
            requests_total += 1;
            accumulator.add_response(response);
            if accumulator.emitted >= 100 && idx + 1 < read.entities.len() {
                accumulator.issue_cap_truncated = true;
                break;
            }
            if accumulator.issue_cap_truncated {
                break;
            }
        }
        Ok(accumulator.into_envelope(read.entity_cap_truncated, requests_total, &endpoint))
    }

    async fn tool_subsystem_members(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let subsystem_id = required_str(arguments, "id")?.to_owned();
        let result = self
            .readers
            .with_reader(move |conn| {
                let Some(subsystem) = entity_by_id(conn, &subsystem_id)? else {
                    return Ok(tool_error_envelope(
                        "entity-not-found",
                        &format!("entity {subsystem_id} was not found"),
                        false,
                    ));
                };
                if subsystem.kind != "subsystem" {
                    return Ok(tool_error_envelope(
                        "not-a-subsystem",
                        &format!("entity {} is kind {}", subsystem.id, subsystem.kind),
                        false,
                    ));
                }
                let members = subsystem_members(conn, &subsystem.id)?
                    .iter()
                    .map(|member| {
                        json!({
                            "id": member.id,
                            "name": member.name,
                            "source_file_path": member.source_file_path
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(success_envelope(json!({
                    "subsystem": {
                        "id": subsystem.id,
                        "name": subsystem.name,
                        "short_name": subsystem.short_name,
                        "properties": entity_properties_json(&subsystem)
                    },
                    "members": members
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    async fn tool_subsystem_of(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let result = self
            .readers
            .with_reader(move |conn| {
                let Some(entity) = entity_by_id(conn, &entity_id)? else {
                    return Ok(tool_error_envelope(
                        "entity-not-found",
                        &format!("entity {entity_id} was not found"),
                        false,
                    ));
                };
                let Some(found) = subsystem_of_entity(conn, &entity.id)? else {
                    // Entity exists but has no subsystem-assigned module ancestor.
                    // A structural fact, not an error — return a success envelope
                    // with subsystem: null so an agent can distinguish it from a
                    // missing entity.
                    return Ok(success_envelope(json!({
                        "entity": {"id": entity.id, "kind": entity.kind},
                        "subsystem": Value::Null,
                        "via_module_id": Value::Null
                    })));
                };
                let subsystem = entity_by_id(conn, &found.subsystem_id)?;
                Ok(success_envelope(json!({
                    "entity": {"id": entity.id, "kind": entity.kind},
                    "subsystem": subsystem.as_ref().map(|s| json!({
                        "id": s.id,
                        "name": s.name,
                        "short_name": s.short_name,
                        "properties": entity_properties_json(s)
                    })),
                    "via_module_id": found.via_module_id
                })))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    async fn tool_call_sites(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let role = match arguments.get("role") {
            None | Some(Value::Null) => CallSiteRole::Caller,
            Some(Value::String(s)) if s == "caller" => CallSiteRole::Caller,
            Some(Value::String(s)) if s == "callee" => CallSiteRole::Callee,
            _ => return Err(ParamError::new("role must be \"caller\" or \"callee\"")),
        };
        let kind = match arguments.get("kind") {
            None | Some(Value::Null) => CallSiteKind::Both,
            Some(Value::String(s)) if s == "calls" => CallSiteKind::Calls,
            Some(Value::String(s)) if s == "references" => CallSiteKind::References,
            _ => return Err(ParamError::new("kind must be \"calls\" or \"references\"")),
        };
        let path = match arguments.get("path") {
            None | Some(Value::Null) => PathScope::All,
            Some(Value::String(s)) if s == "all" => PathScope::All,
            Some(Value::String(s)) if s == "production" => PathScope::Production,
            Some(Value::String(s)) if s == "test" => PathScope::Test,
            _ => {
                return Err(ParamError::new(
                    "path must be \"all\", \"production\", or \"test\"",
                ));
            }
        };
        let confidence = optional_confidence(arguments)?;
        let result = self
            .readers
            .with_reader(move |conn| {
                build_call_sites(conn, &entity_id, role, kind, confidence, path)
            })
            .await;
        match result {
            Ok(Some(value)) => Ok(success_envelope(value)),
            Ok(None) => Ok(tool_error_envelope(
                "not-found",
                "no entity with the given id",
                false,
            )),
            Err(err) => Ok(tool_error_envelope(
                "storage-error",
                &err.to_string(),
                storage_retryable(&err),
            )),
        }
    }

    #[allow(clippy::too_many_lines, clippy::similar_names)]
    async fn tool_orientation_pack(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        // Exactly one resolution form: an `entity` id, or a `file` + `line`.
        let entity_arg = arguments
            .get("entity")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let file_arg = arguments
            .get("file")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let has_line = arguments.get("line").is_some();

        // `query_line == Some` selects the file/line form; `None` the entity form.
        let (query_line, normalized_path, entity_id_arg) = match (entity_arg, file_arg, has_line) {
            (Some(id), None, false) => (None, None, Some(id.to_owned())),
            (None, Some(file), true) => {
                let line = required_i64(arguments, "line")?;
                if line <= 0 {
                    return Err(ParamError::new("line must be a positive integer"));
                }
                match normalize_source_path(&self.project_root, file) {
                    Ok(path) => (Some(line), Some(path), None),
                    Err(err) => {
                        return Ok(tool_error_envelope("invalid-path", &err.to_string(), false));
                    }
                }
            }
            _ => {
                return Err(ParamError::new(
                    "provide exactly one of: `entity` (id), or `file` + `line`",
                ));
            }
        };

        let project_root = self.project_root.clone();
        let edge_cap = self.execution_edge_cap;
        let path_cap = self.execution_path_cap;

        let core = self
            .readers
            .with_reader(move |conn| {
                // Resolve the primary entity. The file/line form additionally
                // yields the containing candidate set for ambiguity reporting.
                let (matched, candidates) = if let Some(line) = query_line {
                    let path = normalized_path.as_deref().unwrap_or_default();
                    let candidates = entities_containing_line(conn, path, line)?;
                    (candidates.first().cloned(), candidates)
                } else {
                    let id = entity_id_arg.as_deref().unwrap_or_default();
                    match entity_by_id(conn, id)? {
                        Some(entity) => (Some(entity.clone()), vec![entity]),
                        None => (None, Vec::new()),
                    }
                };

                let snapshot = crate::snapshot::project_snapshot(conn, &project_root);
                let freshness = json!({
                    "staleness": snapshot.staleness(),
                    "last_analyzed_at": snapshot.last_analyzed_at(),
                    "degraded": snapshot.degraded(),
                });
                let staleness_stale =
                    matches!(snapshot.staleness(), crate::snapshot::Staleness::Stale);

                let Some(entity) = matched else {
                    return Ok(OrientationCore {
                        primary_id: None,
                        primary_kind: None,
                        lookup_was_id: query_line.is_none(),
                        packet: json!({
                            "primary_entity": Value::Null,
                            "entity_context":
                                entity_context_json(query_line, None, &[], &[], &snapshot),
                            "source": Value::Null,
                            "neighbors": Value::Null,
                            "execution_paths": Value::Null,
                        }),
                        freshness,
                        staleness_stale,
                        neighbors_omitted: serde_json::Map::new(),
                        paths_truncation_reason: None,
                    });
                };

                let ancestors = ancestor_chain(conn, &entity.id)?;
                let entity_context = entity_context_json(
                    query_line,
                    Some(&entity),
                    &candidates,
                    &ancestors,
                    &snapshot,
                );

                let source = json!({
                    "source_file_path": entity.source_file_path,
                    "source_line_start": entity.source_line_start,
                    "source_line_end": entity.source_line_end,
                    "line_count": match (entity.source_line_start, entity.source_line_end) {
                        (Some(start), Some(end)) if end >= start => Some(end - start + 1),
                        _ => None,
                    },
                    "content_hash": entity.content_hash,
                });

                // One-hop neighbors at resolved confidence, each bounded.
                let confidence = EdgeConfidence::Resolved;
                let callers_all = call_edges_targeting(conn, &entity.id, confidence)?
                    .into_iter()
                    .filter_map(|edge| caller_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let callees_all = call_edges_from(conn, &entity.id, confidence)?
                    .into_iter()
                    .filter_map(|edge| callee_json(conn, &edge).transpose())
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let container = entity
                    .parent_id
                    .as_deref()
                    .and_then(|parent_id| entity_by_id(conn, parent_id).transpose())
                    .transpose()?
                    .as_ref()
                    .map(entity_json);
                let contained_all = child_entity_ids(conn, &entity.id)?
                    .iter()
                    .filter_map(|child_id| entity_by_id(conn, child_id).transpose())
                    .map(|row| row.map(|entity| entity_json(&entity)))
                    .collect::<Result<Vec<_>, StorageError>>()?;
                let (refs_in, references_rolled_up) = reference_neighbors_for(
                    conn,
                    &entity.id,
                    &entity.kind,
                    ReferenceDirection::In,
                )?;
                let (refs_out, _) = reference_neighbors_for(
                    conn,
                    &entity.id,
                    &entity.kind,
                    ReferenceDirection::Out,
                )?;
                let imports_in = import_neighbors(conn, &entity.id, ReferenceDirection::In)?;
                let imports_out = import_neighbors(conn, &entity.id, ReferenceDirection::Out)?;

                let cap = ORIENTATION_PACK_MAX_NEIGHBORS;
                let (callers, callers_omitted) = cap_neighbor_list(callers_all, cap);
                let (callees, callees_omitted) = cap_neighbor_list(callees_all, cap);
                let (contained, contained_omitted) = cap_neighbor_list(contained_all, cap);
                let (references_in, refs_in_omitted) = cap_neighbor_list(refs_in, cap);
                let (references_out, refs_out_omitted) = cap_neighbor_list(refs_out, cap);
                let (imports_in, imports_in_omitted) = cap_neighbor_list(imports_in, cap);
                let (imports_out, imports_out_omitted) = cap_neighbor_list(imports_out, cap);

                let scope_excludes = call_graph_scope_excludes(confidence);

                let neighbors = json!({
                    "callers": callers,
                    "callees": callees,
                    "container": container,
                    "contained": contained,
                    "references_in": references_in,
                    "references_out": references_out,
                    // See `tool_neighborhood`: module references_in/out are
                    // rolled up over contained symbols (clarion-79d0ff6e14).
                    "references_rolled_up": references_rolled_up,
                    "imports_in": imports_in,
                    "imports_out": imports_out,
                    "scope_excludes": scope_excludes,
                });

                // Compact resolved execution paths.
                let mut traversal = PathTraversal::new(edge_cap);
                let mut path = vec![entity.id.clone()];
                traversal.walk(
                    conn,
                    &entity.id,
                    &mut path,
                    ORIENTATION_PACK_PATH_DEPTH,
                    confidence,
                )?;
                let edge_truncated = traversal.truncated;
                let edge_count_visited = traversal.edge_count_visited;
                let compact = compact_execution_paths(conn, traversal.paths, path_cap)?;
                let paths_truncation_reason =
                    path_truncation_reason(edge_truncated, compact.path_cap_truncated);
                let execution_paths = json!({
                    "root": entity.id,
                    "nodes": compact.nodes,
                    "paths": compact.paths,
                    "edge_count_visited": edge_count_visited,
                    "truncated": paths_truncation_reason.is_some(),
                    "truncation_reason": paths_truncation_reason,
                });

                let mut neighbors_omitted = serde_json::Map::new();
                for (key, omitted) in [
                    ("callers", callers_omitted),
                    ("callees", callees_omitted),
                    ("contained", contained_omitted),
                    ("references_in", refs_in_omitted),
                    ("references_out", refs_out_omitted),
                    ("imports_in", imports_in_omitted),
                    ("imports_out", imports_out_omitted),
                ] {
                    neighbors_omitted.insert(key.to_owned(), json!(omitted));
                }

                Ok(OrientationCore {
                    primary_id: Some(entity.id.clone()),
                    primary_kind: Some(entity.kind.clone()),
                    lookup_was_id: query_line.is_none(),
                    packet: json!({
                        "primary_entity": entity_json(&entity),
                        "entity_context": entity_context,
                        "source": source,
                        "neighbors": neighbors,
                        "execution_paths": execution_paths,
                    }),
                    freshness,
                    staleness_stale,
                    neighbors_omitted,
                    paths_truncation_reason: paths_truncation_reason.map(str::to_owned),
                })
            })
            .await;

        let core = match core {
            Ok(core) => core,
            Err(err) => {
                return Ok(tool_error_envelope(
                    "storage-error",
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };

        // An `entity`-id lookup that resolved to nothing is a hard error; a
        // file/line lookup that spans nothing degrades to a no_match packet.
        if core.primary_id.is_none() && core.lookup_was_id {
            return Ok(tool_error_envelope(
                "entity-not-found",
                "no entity with the given id",
                false,
            ));
        }

        // Related Filigree issues — reuse `issues_for` so its disabled /
        // unreachable degradation paths are shared. Bounded to the primary
        // entity (no contained fan-out) to keep the packet small.
        let issues = if let Some(primary_id) = &core.primary_id {
            let mut issue_args = serde_json::Map::new();
            issue_args.insert("id".to_owned(), json!(primary_id));
            issue_args.insert("include_contained".to_owned(), json!(false));
            match self.tool_issues_for(&issue_args).await {
                Ok(envelope) => envelope.get("result").cloned().unwrap_or(Value::Null),
                Err(_) => json!({"available": false, "reason": "issues lookup failed"}),
            }
        } else {
            json!({"available": false, "reason": "no primary entity at this location"})
        };

        let health = json!({
            "index": core.freshness,
            "filigree": self.filigree_diagnostics_json(),
            "llm": self.llm_diagnostics_json(),
        });

        let neighbors_truncated = core
            .neighbors_omitted
            .values()
            .any(|value| value.as_u64().unwrap_or(0) > 0);
        let paths_truncated = core.paths_truncation_reason.is_some();

        let mut warnings: Vec<String> = Vec::new();
        if core.primary_id.is_none() {
            warnings.push(
                "No entity spans this location; only the enclosing scope (if any) is reported — \
                 not a guaranteed absence of code."
                    .to_owned(),
            );
        }
        if core.staleness_stale {
            warnings.push(
                "Index is stale: at least one ingested source file is newer than the last \
                 analyze run. Re-run `clarion analyze`."
                    .to_owned(),
            );
        }
        if issues.get("available") == Some(&Value::Bool(false)) {
            let reason = issues
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unavailable");
            warnings.push(format!(
                "Filigree issues unavailable ({reason}); the related-issues section is empty for \
                 lack of data, not lack of issues."
            ));
        }
        if neighbors_truncated {
            warnings
                .push("Some neighbor lists were truncated; see `omitted` for counts.".to_owned());
        }
        if paths_truncated {
            warnings.push(
                "Execution paths were truncated; see `omitted.execution_paths_truncation_reason`."
                    .to_owned(),
            );
        }

        let suggested = orientation_suggested_reads(
            &core.packet,
            core.primary_id.as_deref(),
            core.primary_kind.as_deref(),
        );

        let mut omitted = core.neighbors_omitted.clone();
        omitted.insert(
            "execution_paths_truncated".to_owned(),
            json!(paths_truncated),
        );
        omitted.insert(
            "execution_paths_truncation_reason".to_owned(),
            json!(core.paths_truncation_reason),
        );

        let truncated = neighbors_truncated || paths_truncated;

        let mut packet = core.packet;
        let object = packet
            .as_object_mut()
            .expect("orientation packet is an object");
        object.insert("issues".to_owned(), issues);
        object.insert("health".to_owned(), health);
        object.insert("warnings".to_owned(), json!(warnings));
        object.insert("suggested_next_reads".to_owned(), json!(suggested));
        object.insert("omitted".to_owned(), Value::Object(omitted));

        Ok(success_envelope_with_truncation(
            packet,
            truncated.then_some("orientation-pack-bounds"),
        ))
    }

    // Uniform async dispatch with the other tools; the body is sync (spawn +
    // registry insert), hence no await.
    #[allow(clippy::unused_async)]
    async fn tool_analyze_start(
        &self,
        _arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let program = match &self.analyze_program {
            Some(program) => program.clone(),
            None => match std::env::current_exe() {
                Ok(path) => path,
                Err(err) => {
                    return Ok(tool_error_envelope(
                        "spawn-failed",
                        &format!("cannot resolve the clarion executable to launch analyze: {err}"),
                        false,
                    ));
                }
            },
        };

        let run_id = uuid::Uuid::new_v4().to_string();
        let runs_dir = self.project_root.join(".clarion").join("runs");
        if let Err(err) = std::fs::create_dir_all(&runs_dir) {
            return Ok(tool_error_envelope(
                "io-error",
                &format!("create runs directory {}: {err}", runs_dir.display()),
                false,
            ));
        }
        let progress_path = runs_dir.join(format!("{run_id}.progress.json"));
        let started_at = (self.clock)();

        let mut registry = self
            .analyze_runs
            .lock()
            .expect("analyze run registry mutex");
        // Reject a concurrent run: a second `clarion analyze` would fail to
        // acquire the project's cross-process lock anyway, so surface it as a
        // clear error rather than spawning a doomed child.
        let already_active = registry
            .values_mut()
            .any(|handle| !handle.cancelled && matches!(handle.child.try_wait(), Ok(None)));
        if already_active {
            return Ok(tool_error_envelope(
                "analyze-already-running",
                "an analyze run is already active for this project; cancel it or wait for it to finish",
                true,
            ));
        }

        let handle = match crate::analyze_runs::spawn_analyze(
            &program,
            &self.project_root,
            &run_id,
            &progress_path,
            started_at,
        ) {
            Ok(handle) => handle,
            Err(err) => {
                return Ok(tool_error_envelope(
                    "spawn-failed",
                    &format!("failed to spawn `clarion analyze`: {err}"),
                    false,
                ));
            }
        };
        let pid = handle.child.id();
        registry.insert(run_id.clone(), handle);
        drop(registry);

        Ok(success_envelope(json!({
            "run_id": run_id,
            "status": "started",
            "pid": pid,
            "progress_file": progress_path.display().to_string(),
        })))
    }

    async fn tool_analyze_status(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let run_id = required_str(arguments, "run_id")?.to_owned();
        let now = (self.clock)();

        // Snapshot the live state under the lock; reap on exit.
        let live = {
            let mut registry = self
                .analyze_runs
                .lock()
                .expect("analyze run registry mutex");
            match registry.get_mut(&run_id) {
                Some(handle) => match handle.child.try_wait() {
                    Ok(None) => LiveRun::Alive {
                        started_at: handle.started_at.clone(),
                        progress_path: handle.progress_path.clone(),
                    },
                    Ok(Some(_)) | Err(_) => LiveRun::Exited {
                        started_at: handle.started_at.clone(),
                        cancelled: handle.cancelled,
                    },
                },
                None => LiveRun::Absent,
            }
        };

        match live {
            LiveRun::Alive {
                started_at,
                progress_path,
            } => {
                let elapsed = elapsed_seconds(&started_at, &now);
                let progress = read_progress_snapshot(&progress_path);
                let (status, heartbeat_at) = match &progress {
                    Some(snapshot) => (
                        "running",
                        snapshot.get("heartbeat_at").and_then(Value::as_str),
                    ),
                    // Spawned but no progress recorded yet (still in discovery /
                    // before the first write).
                    None => ("queued", None),
                };
                let observed = heartbeat_at
                    .is_some_and(|hb| progress_observed(hb, &now, ANALYZE_HEARTBEAT_STALE_SECS));
                Ok(success_envelope(json!({
                    "run_id": run_id,
                    "status": status,
                    "phase": progress.as_ref().and_then(|p| p.get("phase").cloned()),
                    "current_plugin": progress.as_ref().and_then(|p| p.get("current_plugin").cloned()),
                    "processed_files": progress.as_ref().and_then(|p| p.get("processed_files").cloned()),
                    "total_files": progress.as_ref().and_then(|p| p.get("total_files").cloned()),
                    "current_file": progress.as_ref().and_then(|p| p.get("current_file").cloned()),
                    "heartbeat_at": heartbeat_at,
                    "elapsed_seconds": elapsed,
                    "progress_observed": observed,
                })))
            }
            LiveRun::Exited {
                started_at,
                cancelled,
            } => {
                let row = self.read_run_row(&run_id).await;
                Ok(self.terminal_status_envelope(&run_id, cancelled, Some(&started_at), &now, row))
            }
            LiveRun::Absent => {
                // Not in the registry — may be a run from a prior session.
                let row = self.read_run_row(&run_id).await;
                match &row {
                    Ok(Some(_)) => {
                        Ok(self.terminal_status_envelope(&run_id, false, None, &now, row))
                    }
                    Ok(None) => Ok(tool_error_envelope(
                        "run-not-found",
                        &format!("no analyze run with id {run_id}"),
                        false,
                    )),
                    Err(err) => Ok(tool_error_envelope(
                        "storage-error",
                        &err.to_string(),
                        storage_retryable(err),
                    )),
                }
            }
        }
    }

    async fn tool_analyze_cancel(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let run_id = required_str(arguments, "run_id")?.to_owned();
        let now = (self.clock)();

        let outcome = {
            let mut registry = self
                .analyze_runs
                .lock()
                .expect("analyze run registry mutex");
            match registry.get_mut(&run_id) {
                Some(handle) => match handle.child.try_wait() {
                    Ok(None) => {
                        crate::analyze_runs::kill_run(handle);
                        CancelOutcome::Cancelled
                    }
                    Ok(Some(_)) | Err(_) => CancelOutcome::AlreadyExited {
                        cancelled: handle.cancelled,
                    },
                },
                None => CancelOutcome::Absent,
            }
        };

        match outcome {
            CancelOutcome::Cancelled => {
                let db_path = self.project_root.join(".clarion").join("clarion.db");
                crate::analyze_runs::mark_run_cancelled_in_db(&db_path, &run_id, &now);
                Ok(success_envelope(json!({
                    "run_id": run_id,
                    "status": "cancelled",
                })))
            }
            // Idempotent: the run already finished — report its real terminal
            // state rather than pretending we cancelled it.
            CancelOutcome::AlreadyExited { cancelled } => {
                let row = self.read_run_row(&run_id).await;
                Ok(self.terminal_status_envelope(&run_id, cancelled, None, &now, row))
            }
            CancelOutcome::Absent => {
                let row = self.read_run_row(&run_id).await;
                match &row {
                    Ok(Some(_)) => {
                        Ok(self.terminal_status_envelope(&run_id, false, None, &now, row))
                    }
                    Ok(None) => Ok(tool_error_envelope(
                        "run-not-found",
                        &format!("no analyze run with id {run_id}"),
                        false,
                    )),
                    Err(err) => Ok(tool_error_envelope(
                        "storage-error",
                        &err.to_string(),
                        storage_retryable(err),
                    )),
                }
            }
        }
    }

    /// Read a run's `(status, stats)` from the `runs` table via the reader pool.
    async fn read_run_row(
        &self,
        run_id: &str,
    ) -> std::result::Result<Option<(String, String)>, StorageError> {
        let run_id = run_id.to_owned();
        self.readers
            .with_reader(move |conn| {
                match conn.query_row(
                    "SELECT status, stats FROM runs WHERE id = ?1",
                    rusqlite::params![run_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                ) {
                    Ok(tuple) => Ok(Some(tuple)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(err) => Err(StorageError::from(err)),
                }
            })
            .await
    }

    /// Build a terminal `analyze_status` envelope from the DB row, honoring a
    /// registry cancel flag and surfacing recorded stats.
    #[allow(clippy::unused_self)]
    fn terminal_status_envelope(
        &self,
        run_id: &str,
        cancelled: bool,
        started_at: Option<&str>,
        now: &str,
        row: std::result::Result<Option<(String, String)>, StorageError>,
    ) -> Value {
        let (db_status, stats) = match row {
            Ok(Some((db_status, stats))) => (Some(db_status), stats),
            Ok(None) => (None, "{}".to_owned()),
            Err(err) => {
                return tool_error_envelope(
                    "storage-error",
                    &err.to_string(),
                    storage_retryable(&err),
                );
            }
        };
        let mapped_status = if cancelled || run_stats_is_cancelled(&stats) {
            "cancelled"
        } else {
            match &db_status {
                Some(value) => map_run_status(value, &stats),
                // Process exited but never recorded a run row.
                None => "failed",
            }
        };
        let stats_value = serde_json::from_str::<Value>(&stats).unwrap_or(Value::Null);
        json!({
            "ok": true,
            "result": {
                "run_id": run_id,
                "status": mapped_status,
                "elapsed_seconds": started_at.and_then(|start| elapsed_seconds(start, now)),
                "stats": stats_value,
            },
            "error": null,
            "diagnostics": [],
            "truncated": false,
            "truncation_reason": null,
            "stats_delta": {}
        })
    }

    async fn tool_source_for_entity(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        // Bounded context window; the schema caps at 200 but clamp defensively.
        let context_lines = optional_usize(arguments, "context_lines")?
            .unwrap_or(10)
            .min(200);
        let id_for_reader = entity_id.clone();
        let entity = self
            .readers
            .with_reader(move |conn| entity_by_id(conn, &id_for_reader))
            .await;
        let entity = match entity {
            Ok(Some(entity)) => entity,
            Ok(None) => {
                return Ok(tool_error_envelope(
                    "not-found",
                    &format!("no entity with id {entity_id}"),
                    false,
                ));
            }
            Err(err) => {
                return Ok(tool_error_envelope(
                    "storage-error",
                    &err.to_string(),
                    storage_retryable(&err),
                ));
            }
        };
        Ok(success_envelope(source_for_entity_json(
            &entity,
            context_lines,
        )))
    }

    async fn tool_summary_preview_cost(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let now = (self.clock)();
        let read = match self
            .read_summary_inputs(entity_id, self.summary_model_id())
            .await
        {
            Ok(read) => read,
            Err(err) => {
                return Ok(tool_error_envelope(
                    "storage-error",
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

    async fn tool_project_status(
        &self,
        _arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let db_path = self.project_root.join(".clarion").join("clarion.db");
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
                Ok((
                    snapshot,
                    edge_count,
                    briefing_blocked,
                    plugins,
                    latest_run,
                    data_version,
                ))
            })
            .await;

        let (snapshot, edge_count, briefing_blocked, plugins, latest_run, data_version) =
            match storage {
                Ok(tuple) => tuple,
                Err(err) => {
                    return Ok(tool_error_envelope(
                        "storage-error",
                        &err.to_string(),
                        storage_retryable(&err),
                    ));
                }
            };

        // The on-disk size, paired with data_version, exposes a swapped or
        // truncated DB the server may still be serving from a stale handle.
        let db_size_bytes = std::fs::metadata(&db_path).map(|meta| meta.len()).ok();

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
            "last_analyzed_at": snapshot.last_analyzed_at(),
            // No analyze-time git SHA is persisted and Clarion has no git
            // integration; report null rather than fabricate one.
            "git_sha": Value::Null,
            "plugins": plugins,
            "llm": self.llm_diagnostics_json(),
            "filigree": self.filigree_diagnostics_json(),
        });

        Ok(success_envelope(result))
    }

    async fn tool_index_diff(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let cap = optional_usize(arguments, "limit")?
            .filter(|n| *n > 0)
            .unwrap_or(index_diff::DEFAULT_MAX_ENTRIES);

        // Git is read read-only and fail-soft, off the async runtime since it
        // shells out.
        let git_root = self.project_root.clone();
        let git = match tokio::task::spawn_blocking(move || index_diff::gather_git_facts(&git_root))
            .await
        {
            Ok(facts) => facts,
            Err(err) => {
                return Ok(tool_error_envelope(
                    "internal",
                    &format!("git fact-gathering task failed: {err}"),
                    true,
                ));
            }
        };

        let project_root = self.project_root.clone();
        let result = self
            .readers
            .with_reader(move |conn| {
                let state = index_diff::read_index_state(conn)?;
                Ok(success_envelope(index_diff::build_report(
                    &project_root,
                    &state,
                    &git,
                    cap,
                )))
            })
            .await;
        Ok(flatten_storage_envelope_result(result))
    }

    fn llm_diagnostics_json(&self) -> Value {
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

    fn filigree_diagnostics_json(&self) -> Value {
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

    async fn read_issues_for_entities(
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
                Ok(Some(IssuesForRead {
                    entities,
                    entity_cap_truncated,
                }))
            })
            .await
    }

    async fn tool_summary(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "id")?.to_owned();
        let now = (self.clock)();
        let read = match self
            .read_summary_inputs(entity_id, self.summary_model_id())
            .await
        {
            Ok(read) => read,
            Err(err) => {
                return Ok(tool_error_envelope(
                    "storage-error",
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
                "llm-disabled",
                "LLM summaries are disabled and no fresh cache row is available",
                false,
            ));
        };
        if !summary_llm.config.enabled {
            return Ok(tool_error_envelope(
                "llm-disabled",
                "LLM summaries are disabled and no fresh cache row is available",
                false,
            ));
        }

        Ok(self.refresh_summary(*ready, summary_llm, now).await)
    }

    async fn ensure_inferred_for_target(
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

    async fn ensure_inferred_for_caller(
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
                "token-ceiling-exceeded",
                "LLM session token ceiling has been reached",
                false,
            ));
        }
        let Some(llm) = self.inference_llm_snapshot() else {
            return Err(InferredDispatchFailure::new(
                "llm-disabled",
                "LLM inferred-edge dispatch is disabled and no cache row is available",
                false,
            ));
        };
        if !llm.config.enabled {
            return Err(InferredDispatchFailure::new(
                "llm-disabled",
                "LLM inferred-edge dispatch is disabled and no cache row is available",
                false,
            ));
        }

        self.coalesced_inferred_dispatch(read.key.clone(), read, llm)
            .await
    }

    async fn read_inferred_inputs(
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

    async fn materialize_cached_inferred(
        &self,
        read: InferredRead,
        mut cached: InferredEdgeCacheEntry,
    ) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        let Some(llm) = self.inference_llm_snapshot() else {
            return Err(InferredDispatchFailure::new(
                "llm-disabled",
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

    async fn coalesced_inferred_dispatch(
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
                    "inferred-dispatch-cancelled",
                    "inferred dispatch owner ended before broadcasting a result",
                    true,
                )),
                Err(_) => Err(InferredDispatchFailure::new(
                    "inferred-dispatch-timeout",
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

    async fn perform_inferred_dispatch(
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
                "token-ceiling-exceeded",
                "LLM session token ceiling has been reached",
                false,
            ));
        };
        let response = invoke_llm_provider(Arc::clone(&llm.provider), request)
            .await
            .map_err(|err| {
                InferredDispatchFailure::new(
                    "llm-provider-error",
                    &err.to_string(),
                    err.retryable(),
                )
            })?;
        if !reservation.commit(
            u64::from(response.total_tokens),
            llm.config.session_token_ceiling,
        ) {
            return Err(InferredDispatchFailure::new(
                "token-ceiling-exceeded",
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
            Err(err) if err.code == "llm-invalid-json" => {
                let message = err.message.clone();
                return Err(err.with_stats(
                    inferred_usage_stats(&response, true),
                    vec![json!({
                        "code": "CLA-LLM-INVALID-JSON",
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
    async fn drop_unresolved_inferred_targets(
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

    async fn read_summary_inputs(
        &self,
        entity_id: String,
        summary_model_id: String,
    ) -> Result<SummaryRead, StorageError> {
        self.readers
            .with_reader(move |conn| {
                let Some(entity) = entity_by_id(conn, &entity_id)? else {
                    return Ok(SummaryRead::EntityNotFound(entity_id));
                };
                if entity.kind == "subsystem" {
                    return Ok(SummaryRead::ScopeDeferred(Box::new(entity)));
                }
                if let Some(reason) = briefing_block_reason(&entity) {
                    return Ok(SummaryRead::BriefingBlocked(Box::new(entity), reason));
                }
                let Some(content_hash) = entity.content_hash.clone() else {
                    return Ok(SummaryRead::MissingContentHash(entity.id));
                };
                let key = SummaryCacheKey {
                    entity_id: entity.id.clone(),
                    content_hash,
                    prompt_template_id: LEAF_SUMMARY_PROMPT_TEMPLATE_ID.to_owned(),
                    model_tier: summary_model_id,
                    guidance_fingerprint: EMPTY_GUIDANCE_FINGERPRINT.to_owned(),
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
                Ok(SummaryRead::Ready(Box::new(SummaryReady {
                    entity,
                    key,
                    cached,
                    caller_count,
                    fan_out,
                })))
            })
            .await
    }

    async fn cached_summary_envelope(&self, ready: &SummaryReady, now: &str) -> Option<Value> {
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
                "storage-error",
                &err.to_string(),
                storage_retryable(&err),
            ));
        }
        Some(summary_success_envelope(
            &ready.entity,
            cached,
            true,
            stale_semantic(cached, ready.caller_count, ready.fan_out),
            None,
            json!({"summary_cache_hits_total": 1}),
        ))
    }

    async fn refresh_summary(
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
                    "llm-provider-error",
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
                    "storage-error",
                    &err.to_string(),
                    storage_retryable(&err),
                );
            }
            return summary_success_envelope(
                &ready.entity,
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
            return tool_error_envelope("storage-error", &err.to_string(), storage_retryable(&err));
        }

        summary_success_envelope(
            &ready.entity,
            &entry,
            false,
            false,
            Some(cached_input_tokens),
            stats_delta,
        )
    }

    async fn send_writer<T>(
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
        ack_rx.await.map_err(|_| StorageError::WriterNoResponse)?
    }

    fn summary_budget_blocked(&self) -> bool {
        self.budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .blocked
    }

    fn reserve_budget(
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

    fn inference_llm_snapshot(&self) -> Option<InferenceLlmState> {
        self.summary_llm.as_ref().map(|llm| InferenceLlmState {
            writer: llm.writer.clone(),
            config: llm.config.clone(),
            provider: Arc::clone(&llm.provider),
        })
    }

    fn summary_cache_max_age_days(&self) -> u32 {
        self.summary_llm
            .as_ref()
            .map_or(180, |summary| summary.config.cache_max_age_days)
    }

    fn summary_model_id(&self) -> String {
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

    fn inferred_edges_model_id(&self) -> String {
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

    fn max_inferred_edges_per_caller(&self) -> usize {
        self.summary_llm.as_ref().map_or(8, |summary| {
            usize::try_from(summary.config.max_inferred_edges_per_caller).unwrap_or(8)
        })
    }
}

async fn invoke_llm_provider(
    provider: Arc<dyn LlmProvider>,
    request: LlmRequest,
) -> Result<LlmResponse, LlmProviderError> {
    tokio::task::spawn_blocking(move || provider.invoke(request))
        .await
        .map_err(|err| LlmProviderError::InvalidResponse {
            message: format!("LLM provider task failed: {err}"),
            retryable: true,
        })?
}

struct SummaryLlmState {
    writer: mpsc::Sender<WriterCmd>,
    config: LlmConfig,
    provider: Arc<dyn LlmProvider>,
}

#[derive(Default)]
struct BudgetLedger {
    spent_tokens: u64,
    reserved_tokens: u64,
    blocked: bool,
}

struct BudgetReservation {
    budget: Arc<Mutex<BudgetLedger>>,
    amount_tokens: u64,
    active: bool,
}

impl BudgetReservation {
    fn commit(mut self, actual_tokens: u64, ceiling_tokens: u64) -> bool {
        let mut budget = self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.active {
            budget.reserved_tokens = budget.reserved_tokens.saturating_sub(self.amount_tokens);
            self.active = false;
        }
        // `budget.blocked` gates *new* reservations, not in-flight commits.
        // A reservation that already cleared reserve_budget paid for its
        // dispatch slot; commit it iff the actual usage fits the ceiling.
        if budget.spent_tokens.saturating_add(actual_tokens) > ceiling_tokens {
            budget.blocked = true;
            return false;
        }
        budget.spent_tokens = budget.spent_tokens.saturating_add(actual_tokens);
        true
    }
}

impl Drop for BudgetReservation {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut budget = self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        budget.reserved_tokens = budget.reserved_tokens.saturating_sub(self.amount_tokens);
        self.active = false;
    }
}

enum SummaryRead {
    Ready(Box<SummaryReady>),
    EntityNotFound(String),
    MissingContentHash(String),
    ScopeDeferred(Box<EntityRow>),
    BriefingBlocked(Box<EntityRow>, String),
}

struct SummaryReady {
    entity: EntityRow,
    key: SummaryCacheKey,
    cached: Option<SummaryCacheEntry>,
    caller_count: i64,
    fan_out: i64,
}

struct IssuesForRead {
    entities: Vec<EntityRow>,
    entity_cap_truncated: bool,
}

struct IssuesForAccumulator {
    entities_by_id: HashMap<String, EntityRow>,
    seen_issue_ids: BTreeSet<String>,
    matched: Vec<Value>,
    drifted: Vec<Value>,
    not_found: Vec<Value>,
    diagnostics: Vec<Value>,
    emitted: usize,
    issue_cap_truncated: bool,
}

impl IssuesForAccumulator {
    fn new(entities: &[EntityRow]) -> Self {
        Self {
            entities_by_id: entities
                .iter()
                .map(|entity| (entity.id.clone(), entity.clone()))
                .collect(),
            seen_issue_ids: BTreeSet::new(),
            matched: Vec::new(),
            drifted: Vec::new(),
            not_found: Vec::new(),
            diagnostics: Vec::new(),
            emitted: 0,
            issue_cap_truncated: false,
        }
    }

    fn add_response(&mut self, response: EntityAssociationsResponse) {
        for association in response.associations {
            if self.emitted >= 100 {
                self.issue_cap_truncated = true;
                break;
            }
            if !self.seen_issue_ids.insert(association.issue_id.clone()) {
                continue;
            }
            self.emitted += 1;
            self.add_association(&association);
        }
    }

    fn add_association(&mut self, association: &EntityAssociation) {
        match self.entities_by_id.get(&association.clarion_entity_id) {
            None => self
                .not_found
                .push(association_json(association, None, None, "not_found")),
            Some(entity) => match entity.content_hash.as_deref() {
                Some(current_hash) if current_hash == association.content_hash_at_attach => {
                    self.matched.push(association_json(
                        association,
                        Some(entity),
                        Some(current_hash),
                        "matched",
                    ));
                }
                Some(current_hash) => {
                    self.drifted.push(association_json(
                        association,
                        Some(entity),
                        Some(current_hash),
                        "drifted",
                    ));
                }
                None => {
                    self.diagnostics.push(json!({
                        "code": "CLA-ENTITY-CONTENT-HASH-MISSING",
                        "entity_id": entity.id
                    }));
                    self.matched
                        .push(association_json(association, Some(entity), None, "unknown"));
                }
            },
        }
    }

    fn into_envelope(
        self,
        entity_cap_truncated: bool,
        requests_total: usize,
        filigree_endpoint: &Value,
    ) -> Value {
        let truncation_reason = if self.issue_cap_truncated {
            Some("issue-cap")
        } else {
            entity_cap_truncated.then_some("entity-cap")
        };
        // result_kind lets a consumer act on the outcome without re-deriving it
        // from array lengths, and — paired with `available: false` from
        // `issues_unavailable` — distinguishes "Filigree reachable but no issues
        // are attached" (no_matches) from "Filigree unreachable/disabled"
        // (unavailable).
        let result_kind = if self.matched.is_empty() && self.drifted.is_empty() {
            "no_matches"
        } else {
            "matched"
        };
        let mut envelope = success_envelope_with_truncation_and_stats(
            json!({
                "available": true,
                "result_kind": result_kind,
                "filigree_endpoint": filigree_endpoint,
                "matched": self.matched,
                "drifted": self.drifted,
                "not_found": self.not_found
            }),
            truncation_reason,
            json!({
                "filigree_requests_total": requests_total,
                "filigree_issues_returned_total": self.emitted
            }),
        );
        if let Some(object) = envelope.as_object_mut()
            && !self.diagnostics.is_empty()
        {
            object.insert("diagnostics".to_owned(), Value::Array(self.diagnostics));
        }
        envelope
    }
}

#[derive(Clone)]
struct InferenceLlmState {
    writer: mpsc::Sender<WriterCmd>,
    config: LlmConfig,
    provider: Arc<dyn LlmProvider>,
}

struct InferredInflightGuard {
    in_flight: InferredInflight,
    key: InferredEdgeCacheKey,
    sender: broadcast::Sender<InferredDispatchOutcome>,
    active: bool,
}

impl InferredInflightGuard {
    fn new(
        in_flight: InferredInflight,
        key: InferredEdgeCacheKey,
        sender: broadcast::Sender<InferredDispatchOutcome>,
    ) -> Self {
        Self {
            in_flight,
            key,
            sender,
            active: true,
        }
    }

    async fn remove(mut self) -> Option<broadcast::Sender<InferredDispatchOutcome>> {
        let removed = remove_matching_inferred_inflight(
            Arc::clone(&self.in_flight),
            self.key.clone(),
            self.sender.clone(),
        )
        .await;
        self.active = false;
        removed
    }
}

impl Drop for InferredInflightGuard {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let in_flight = Arc::clone(&self.in_flight);
        let key = self.key.clone();
        let sender = self.sender.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = remove_matching_inferred_inflight(in_flight, key, sender).await;
            });
        }
    }
}

async fn remove_matching_inferred_inflight(
    in_flight: InferredInflight,
    key: InferredEdgeCacheKey,
    sender: broadcast::Sender<InferredDispatchOutcome>,
) -> Option<broadcast::Sender<InferredDispatchOutcome>> {
    let mut map = in_flight.lock().await;
    if map
        .get(&key)
        .is_some_and(|current| current.same_channel(&sender))
    {
        map.remove(&key)
    } else {
        None
    }
}

#[derive(Clone)]
struct InferredRead {
    caller: EntityRow,
    sites: Vec<UnresolvedCallSiteRow>,
    candidates: Vec<EntityRow>,
    key: InferredEdgeCacheKey,
    cached: Option<InferredEdgeCacheEntry>,
}

#[derive(Debug, Clone, Default)]
struct InferredDispatchStats {
    cache_hits_total: u64,
    cache_misses_total: u64,
    edges_materialized_total: u64,
    edges_skipped_static_duplicates_total: u64,
    /// LLM-proposed `to_id` values that did not resolve in the `entities`
    /// table at write time (clarion-df58379de4). Counted here, dropped from
    /// the persisted edge set to avoid the FK violation that previously
    /// poisoned the cache row and re-burned LLM tokens on retry.
    unresolved_targets_dropped_total: u64,
    briefing_blocked_total: u64,
    candidate_callers_considered: u64,
    coalesced_waits_total: u64,
    tokens_input: i64,
    tokens_cached_input: i64,
    tokens_output: i64,
    tokens_total: i64,
    cost_usd: f64,
}

impl InferredDispatchStats {
    fn cache_hit(write: InferredEdgeWriteStats) -> Self {
        Self {
            cache_hits_total: 1,
            edges_materialized_total: write.inserted_edges,
            edges_skipped_static_duplicates_total: write.skipped_static_duplicates,
            ..Self::default()
        }
    }

    fn cache_miss(write: InferredEdgeWriteStats, response: &LlmResponse) -> Self {
        Self {
            cache_misses_total: 1,
            edges_materialized_total: write.inserted_edges,
            edges_skipped_static_duplicates_total: write.skipped_static_duplicates,
            tokens_input: i64::from(response.input_tokens),
            tokens_cached_input: i64::from(response.cached_input_tokens),
            tokens_output: i64::from(response.output_tokens),
            tokens_total: i64::from(response.total_tokens),
            cost_usd: response.cost_usd,
            ..Self::default()
        }
    }

    fn briefing_blocked() -> Self {
        Self {
            briefing_blocked_total: 1,
            ..Self::default()
        }
    }

    fn merge(&mut self, other: &Self) {
        self.cache_hits_total += other.cache_hits_total;
        self.cache_misses_total += other.cache_misses_total;
        self.edges_materialized_total += other.edges_materialized_total;
        self.edges_skipped_static_duplicates_total += other.edges_skipped_static_duplicates_total;
        self.unresolved_targets_dropped_total += other.unresolved_targets_dropped_total;
        self.briefing_blocked_total += other.briefing_blocked_total;
        self.candidate_callers_considered += other.candidate_callers_considered;
        self.coalesced_waits_total += other.coalesced_waits_total;
        self.tokens_input += other.tokens_input;
        self.tokens_cached_input += other.tokens_cached_input;
        self.tokens_output += other.tokens_output;
        self.tokens_total += other.tokens_total;
        self.cost_usd += other.cost_usd;
    }

    fn to_json(&self) -> Value {
        json!({
            "inferred_dispatch_cache_hits_total": self.cache_hits_total,
            "inferred_dispatch_misses_total": self.cache_misses_total,
            "inferred_edges_materialized_total": self.edges_materialized_total,
            "inferred_edges_skipped_static_duplicates_total": self.edges_skipped_static_duplicates_total,
            "inferred_unresolved_targets_dropped_total": self.unresolved_targets_dropped_total,
            "inferred_dispatch_briefing_blocked_total": self.briefing_blocked_total,
            "inferred_candidate_callers_considered": self.candidate_callers_considered,
            "inferred_dispatch_coalesced_total": self.coalesced_waits_total,
            "inferred_tokens_input": self.tokens_input,
            "inferred_tokens_cached_input": self.tokens_cached_input,
            "inferred_tokens_output": self.tokens_output,
            "inferred_tokens_total": self.tokens_total,
            "inferred_cost_usd": self.cost_usd
        })
    }
}

#[derive(Debug, Clone)]
struct InferredDispatchFailure {
    code: &'static str,
    message: String,
    retryable: bool,
    stats_delta: Value,
    diagnostics: Vec<Value>,
}

impl InferredDispatchFailure {
    fn new(code: &'static str, message: &str, retryable: bool) -> Self {
        Self {
            code,
            message: message.to_owned(),
            retryable,
            stats_delta: json!({}),
            diagnostics: Vec::new(),
        }
    }

    fn from_storage(err: &StorageError) -> Self {
        // FK violations are deterministic against the same row set; treating
        // them as `retryable=true` causes the client to re-issue the LLM call
        // and re-pay the token cost (clarion-df58379de4). Mark them
        // non-retryable so a client honouring the hint gives up immediately.
        Self {
            code: "storage-error",
            message: err.to_string(),
            retryable: !err.is_foreign_key_violation(),
            stats_delta: json!({}),
            diagnostics: Vec::new(),
        }
    }

    fn with_stats(mut self, stats_delta: Value, diagnostics: Vec<Value>) -> Self {
        self.stats_delta = stats_delta;
        self.diagnostics = diagnostics;
        self
    }

    fn to_envelope(&self) -> Value {
        if self.code == "token-ceiling-exceeded" {
            return token_ceiling_envelope(&self.message);
        }
        tool_error_envelope_with_diagnostics(
            self.code,
            &self.message,
            self.retryable,
            self.stats_delta.clone(),
            self.diagnostics.clone(),
        )
    }
}

#[derive(Debug, Clone)]
enum InferredDispatchOutcome {
    Ok(InferredDispatchStats),
    Err(InferredDispatchFailure),
}

impl InferredDispatchOutcome {
    fn from_result(result: Result<InferredDispatchStats, InferredDispatchFailure>) -> Self {
        match result {
            Ok(stats) => Self::Ok(stats),
            Err(err) => Self::Err(err),
        }
    }

    fn into_result(self) -> Result<InferredDispatchStats, InferredDispatchFailure> {
        match self {
            Self::Ok(stats) => Ok(stats),
            Self::Err(err) => Err(err),
        }
    }
}

#[derive(Debug, Deserialize)]
struct InferredCallsResponse {
    #[serde(default)]
    edges: Vec<InferredCallsResponseEdge>,
}

#[derive(Debug, Deserialize)]
struct InferredCallsResponseEdge {
    site_key: Option<String>,
    target_id: String,
    confidence: Option<f64>,
    rationale: Option<String>,
}

#[derive(Debug, Error)]
pub enum McpError {
    #[error("invalid JSON-RPC frame body: {0}")]
    Json(#[from] serde_json::Error),

    #[error("MCP transport error: {0}")]
    Transport(#[from] TransportError),

    #[error("MCP runtime error: {0}")]
    Runtime(#[from] std::io::Error),
}

/// Decode and handle a state-free MCP frame.
///
/// Storage-backed tool calls require [`handle_frame_with_state`].
pub fn handle_frame(frame: &Frame) -> Result<Option<Frame>, McpError> {
    let request = serde_json::from_slice(&frame.body)?;
    let Some(response) = handle_json_rpc(&request) else {
        return Ok(None);
    };
    Ok(Some(encode_response_frame(&response)?))
}

pub async fn handle_frame_with_state(
    state: &ServerState,
    frame: &Frame,
) -> Result<Option<Frame>, McpError> {
    let request = serde_json::from_slice(&frame.body)?;
    let Some(response) = state.handle_json_rpc(&request).await else {
        return Ok(None);
    };
    Ok(Some(encode_response_frame(&response)?))
}

fn handle_stdio_frame(frame: &Frame) -> Result<Option<Frame>, McpError> {
    handle_frame(frame)
}

async fn handle_stdio_frame_with_state(
    state: &ServerState,
    frame: &Frame,
) -> Result<Option<Frame>, McpError> {
    handle_frame_with_state(state, frame).await
}

fn encode_response_frame(response: &Value) -> Result<Frame, McpError> {
    Ok(Frame {
        body: serde_json::to_vec(response)?,
    })
}

fn is_json_rpc_notification(request: &Value) -> bool {
    request
        .as_object()
        .is_some_and(|object| object.get("method").is_some() && object.get("id").is_none())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdioFraming {
    ContentLength,
    JsonLine,
}

struct StdioFrame {
    body: Vec<u8>,
    framing: StdioFraming,
}

fn read_stdio_frame(reader: &mut impl std::io::BufRead) -> Result<Option<StdioFrame>, McpError> {
    let Some(first_byte) = peek_stdio_frame_start(reader)? else {
        return Ok(None);
    };
    if first_byte == b'{' || first_byte == b'[' || first_byte.is_ascii_whitespace() {
        return Ok(Some(read_json_line_frame(reader)?));
    }
    match clarion_core::plugin::read_frame(reader, ContentLengthCeiling::DEFAULT) {
        Ok(frame) => Ok(Some(StdioFrame {
            body: frame.body,
            framing: StdioFraming::ContentLength,
        })),
        Err(TransportError::Io(err)) if err.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn peek_stdio_frame_start(reader: &mut impl std::io::BufRead) -> Result<Option<u8>, McpError> {
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(None);
        }
        let blank_prefix = buffer
            .iter()
            .take_while(|byte| matches!(byte, b'\r' | b'\n'))
            .count();
        if blank_prefix == 0 {
            return Ok(Some(buffer[0]));
        }
        reader.consume(blank_prefix);
    }
}

fn read_json_line_frame(reader: &mut impl std::io::BufRead) -> Result<StdioFrame, McpError> {
    let mut body = Vec::new();
    let read = std::io::BufRead::read_until(reader, b'\n', &mut body)?;
    if read == 0 {
        return Err(TransportError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "EOF while reading MCP JSON line",
        ))
        .into());
    }
    while matches!(body.last(), Some(b'\n' | b'\r')) {
        body.pop();
    }
    Ok(StdioFrame {
        body,
        framing: StdioFraming::JsonLine,
    })
}

fn write_stdio_response(
    writer: &mut impl std::io::Write,
    response: &Frame,
    framing: StdioFraming,
) -> Result<(), McpError> {
    match framing {
        StdioFraming::ContentLength => {
            clarion_core::plugin::write_frame(writer, response)?;
        }
        StdioFraming::JsonLine => {
            writer.write_all(&response.body)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
        }
    }
    Ok(())
}

/// Serve state-free MCP protocol metadata over stdio.
///
/// Storage-backed tool calls require [`serve_stdio_with_state`].
pub fn serve_stdio(
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
) -> Result<(), McpError> {
    loop {
        let Some(frame) = read_stdio_frame(reader)? else {
            return Ok(());
        };
        let framing = frame.framing;
        if let Some(response) = handle_stdio_frame(&Frame { body: frame.body })? {
            write_stdio_response(writer, &response, framing)?;
        }
    }
}

pub fn serve_stdio_with_state(
    state: &ServerState,
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
) -> Result<(), McpError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    serve_stdio_with_state_on_runtime(&runtime, state, reader, writer)
}

pub fn serve_stdio_with_state_on_runtime(
    runtime: &tokio::runtime::Runtime,
    state: &ServerState,
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
) -> Result<(), McpError> {
    let _guard = runtime.enter();
    loop {
        let Some(frame) = read_stdio_frame(reader)? else {
            return Ok(());
        };
        let framing = frame.framing;
        if let Some(response) = runtime.block_on(handle_stdio_frame_with_state(
            state,
            &Frame { body: frame.body },
        ))? {
            write_stdio_response(writer, &response, framing)?;
        }
    }
}

/// Build the `initialize` result, advertising only the capabilities the
/// handling path actually serves. The stateless free [`handle_json_rpc`] serves
/// `tools` only (it returns method-not-found for `resources/*` and `prompts/*`),
/// so it passes `stateful = false`; [`ServerState::handle_json_rpc`] serves the
/// full surface and passes `stateful = true`. The `instructions` field is static
/// orientation guidance (not a capability) and is included in both.
fn initialize_result(stateful: bool) -> Value {
    let capabilities = if stateful {
        json!({ "tools": {}, "prompts": {}, "resources": {} })
    } else {
        json!({ "tools": {} })
    };
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": capabilities,
        "serverInfo": {
            "name": "clarion",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": server_instructions()
    })
}

fn resources_list() -> Value {
    json!({
        "resources": [
            {
                "uri": "clarion://context",
                "name": "Clarion project context",
                "description": "Live entity / subsystem / finding counts and index freshness for this project.",
                "mimeType": "application/json"
            }
        ]
    })
}

fn prompts_list() -> Value {
    json!({
        "prompts": [
            {
                "name": "clarion-workflow",
                "description": "How to use Clarion's MCP tools to navigate this codebase."
            }
        ]
    })
}

fn prompts_get(id: &Value, params: Option<&Value>) -> Value {
    let name = params
        .and_then(Value::as_object)
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str);
    if name != Some("clarion-workflow") {
        return error_response(id, -32602, "unknown prompt");
    }
    result_response(
        id,
        &json!({
            "description": "How to use Clarion's MCP tools to navigate this codebase.",
            "messages": [
                {
                    "role": "user",
                    "content": { "type": "text", "text": CLARION_WORKFLOW_SKILL }
                }
            ]
        }),
    )
}

#[derive(Debug)]
struct ParamError {
    message: String,
}

impl ParamError {
    fn new(message: &str) -> Self {
        Self {
            message: message.to_owned(),
        }
    }

    fn to_json_rpc(&self, id: &Value) -> Value {
        error_response(id, -32602, &self.message)
    }
}

struct PathTraversal {
    edge_cap: usize,
    edge_count_visited: usize,
    truncated: bool,
    paths: Vec<Vec<String>>,
}

impl PathTraversal {
    fn new(edge_cap: usize) -> Self {
        Self {
            edge_cap,
            edge_count_visited: 0,
            truncated: false,
            paths: Vec::new(),
        }
    }

    fn walk(
        &mut self,
        conn: &rusqlite::Connection,
        current_id: &str,
        path: &mut Vec<String>,
        remaining_depth: usize,
        confidence: EdgeConfidence,
    ) -> Result<(), StorageError> {
        if remaining_depth == 0 || self.truncated {
            return Ok(());
        }
        for edge in call_edges_from(conn, current_id, confidence)? {
            self.edge_count_visited += 1;
            if self.edge_count_visited > self.edge_cap {
                self.truncated = true;
                return Ok(());
            }
            if path.iter().any(|seen| seen == &edge.to_id) {
                continue;
            }
            path.push(edge.to_id.clone());
            self.paths.push(path.clone());
            self.walk(conn, &edge.to_id, path, remaining_depth - 1, confidence)?;
            path.pop();
            if self.truncated {
                return Ok(());
            }
        }
        Ok(())
    }
}

fn required_str<'a>(
    arguments: &'a serde_json::Map<String, Value>,
    field: &str,
) -> std::result::Result<&'a str, ParamError> {
    arguments
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ParamError::new(&format!("{field} must be a non-empty string")))
}

fn required_i64(
    arguments: &serde_json::Map<String, Value>,
    field: &str,
) -> std::result::Result<i64, ParamError> {
    arguments
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| ParamError::new(&format!("{field} must be an integer")))
}

fn optional_usize(
    arguments: &serde_json::Map<String, Value>,
    field: &str,
) -> std::result::Result<Option<usize>, ParamError> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
    let Some(raw) = value.as_u64() else {
        return Err(ParamError::new(&format!(
            "{field} must be a non-negative integer"
        )));
    };
    usize::try_from(raw)
        .map(Some)
        .map_err(|_| ParamError::new(&format!("{field} is too large")))
}

fn optional_bool(
    arguments: &serde_json::Map<String, Value>,
    field: &str,
) -> std::result::Result<Option<bool>, ParamError> {
    let Some(value) = arguments.get(field) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| ParamError::new(&format!("{field} must be a boolean")))
}

fn optional_confidence(
    arguments: &serde_json::Map<String, Value>,
) -> std::result::Result<EdgeConfidence, ParamError> {
    match arguments.get("confidence").and_then(Value::as_str) {
        None | Some("resolved") => Ok(EdgeConfidence::Resolved),
        Some("ambiguous") => Ok(EdgeConfidence::Ambiguous),
        Some("inferred") => Ok(EdgeConfidence::Inferred),
        Some(_) => Err(ParamError::new(
            "confidence must be one of resolved, ambiguous, inferred",
        )),
    }
}

/// Call-graph blind spots a query did **not** search, so a consumer never reads
/// an empty or partial caller/path result as a true negative (clarion-0d204a3f16).
/// The static resolver cannot bind a call made through an attribute receiver
/// (e.g. `ctx.orchestrator.resume()`); only `inferred` (LLM) dispatch attempts
/// those, so `resolved`/`ambiguous` queries exclude them and `inferred` does not.
fn call_graph_scope_excludes(confidence: EdgeConfidence) -> Vec<&'static str> {
    match confidence {
        EdgeConfidence::Resolved | EdgeConfidence::Ambiguous => vec!["attribute-receiver-calls"],
        EdgeConfidence::Inferred => Vec::new(),
    }
}

fn envelope_from_storage_result(result: Result<Value, StorageError>) -> Value {
    match result {
        Ok(result) => success_envelope(result),
        Err(err) => tool_error_envelope("storage-error", &err.to_string(), storage_retryable(&err)),
    }
}

/// Fail-soft scalar count for `project_status`: a query hiccup degrades to 0
/// (logged), never failing the diagnostics tool. Matches `snapshot.rs`'s policy.
fn scalar_count_fail_soft(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get::<_, i64>(0))
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, sql, "project_status count query failed; reporting 0");
            0
        })
}

/// Per-plugin entity counts from the current index (`entities.plugin_id`), the
/// queryable proxy for "which plugins produced this graph." Fail-soft: any
/// query error degrades to an empty array (logged).
fn plugin_entity_counts(conn: &rusqlite::Connection) -> Value {
    let mut stmt = match conn
        .prepare("SELECT plugin_id, COUNT(*) FROM entities GROUP BY plugin_id ORDER BY plugin_id")
    {
        Ok(stmt) => stmt,
        Err(err) => {
            tracing::warn!(error = %err, "project_status plugin-count prepare failed");
            return Value::Array(Vec::new());
        }
    };
    let rows = stmt.query_map([], |row| {
        Ok(json!({
            "plugin_id": row.get::<_, String>(0)?,
            "entity_count": row.get::<_, i64>(1)?,
        }))
    });
    match rows {
        Ok(mapped) => Value::Array(mapped.filter_map(Result::ok).collect()),
        Err(err) => {
            tracing::warn!(error = %err, "project_status plugin-count query failed");
            Value::Array(Vec::new())
        }
    }
}

/// The most-recent run by `started_at`, regardless of terminal status, so a
/// `skipped_no_plugins` or `failed` run is visible (not just the last
/// `completed` one). Fail-soft: no rows or a query error → `null`.
fn latest_run_row(conn: &rusqlite::Connection) -> Value {
    match conn.query_row(
        "SELECT id, status, started_at, completed_at FROM runs \
         ORDER BY started_at DESC LIMIT 1",
        [],
        |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "status": row.get::<_, String>(1)?,
                "started_at": row.get::<_, String>(2)?,
                "completed_at": row.get::<_, Option<String>>(3)?,
            }))
        },
    ) {
        Ok(value) => value,
        Err(rusqlite::Error::QueryReturnedNoRows) => Value::Null,
        Err(err) => {
            tracing::warn!(error = %err, "project_status latest-run query failed");
            Value::Null
        }
    }
}

fn flatten_storage_envelope_result(result: Result<Value, StorageError>) -> Value {
    match result {
        Ok(envelope) => envelope,
        Err(err) => tool_error_envelope("storage-error", &err.to_string(), storage_retryable(&err)),
    }
}

/// `storage-error` retryable hint. FK violations are deterministic against
/// the same row set; everything else (`SQLITE_BUSY`, disk-full, pool errors)
/// stays retryable (clarion-df58379de4).
fn storage_retryable(err: &StorageError) -> bool {
    !err.is_foreign_key_violation()
}

fn success_envelope(result: Value) -> Value {
    success_envelope_with_truncation(result, None)
}

fn success_envelope_with_truncation(result: Value, truncation_reason: Option<&str>) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert("ok".to_owned(), Value::Bool(true));
    envelope.insert("result".to_owned(), result);
    envelope.insert("error".to_owned(), Value::Null);
    envelope.insert("diagnostics".to_owned(), Value::Array(Vec::new()));
    envelope.insert(
        "truncated".to_owned(),
        Value::Bool(truncation_reason.is_some()),
    );
    envelope.insert(
        "truncation_reason".to_owned(),
        truncation_reason.map_or(Value::Null, |reason| Value::String(reason.to_owned())),
    );
    envelope.insert("stats_delta".to_owned(), json!({}));
    Value::Object(envelope)
}

fn success_envelope_with_truncation_and_stats(
    result: Value,
    truncation_reason: Option<&str>,
    stats_delta: Value,
) -> Value {
    let mut envelope = success_envelope_with_truncation(result, truncation_reason);
    if let Some(object) = envelope.as_object_mut() {
        object.insert("stats_delta".to_owned(), stats_delta);
    }
    envelope
}

fn success_envelope_with_stats(result: Value, stats_delta: Value) -> Value {
    let mut envelope = success_envelope(result);
    if let Some(object) = envelope.as_object_mut() {
        object.insert("stats_delta".to_owned(), stats_delta);
    }
    envelope
}

fn tool_error_envelope(code: &str, message: &str, retryable: bool) -> Value {
    tool_error_envelope_with_diagnostics(code, message, retryable, json!({}), Vec::new())
}

fn tool_error_envelope_with_diagnostics(
    code: &str,
    message: &str,
    retryable: bool,
    stats_delta: Value,
    diagnostics: Vec<Value>,
) -> Value {
    let mut envelope = serde_json::Map::new();
    envelope.insert("ok".to_owned(), Value::Bool(false));
    envelope.insert("result".to_owned(), Value::Null);
    envelope.insert(
        "error".to_owned(),
        json!({
            "code": code,
            "message": message,
            "retryable": retryable,
        }),
    );
    envelope.insert("diagnostics".to_owned(), Value::Array(diagnostics));
    envelope.insert("truncated".to_owned(), Value::Bool(false));
    envelope.insert("truncation_reason".to_owned(), Value::Null);
    envelope.insert("stats_delta".to_owned(), stats_delta);
    Value::Object(envelope)
}

fn llm_usage_json(response: &LlmResponse) -> Value {
    json!({
        "tokens_input": response.input_tokens,
        "tokens_cached_input": response.cached_input_tokens,
        "tokens_output": response.output_tokens,
        "tokens_total": response.total_tokens,
        "cost_usd": response.cost_usd
    })
}

fn summary_usage_stats(response: &LlmResponse, invalid_json: bool) -> Value {
    let mut stats = serde_json::Map::new();
    stats.insert("summary_cache_misses_total".to_owned(), json!(1));
    stats.insert(
        "summary_tokens_input".to_owned(),
        json!(response.input_tokens),
    );
    stats.insert(
        "summary_tokens_cached_input".to_owned(),
        json!(response.cached_input_tokens),
    );
    stats.insert(
        "summary_tokens_output".to_owned(),
        json!(response.output_tokens),
    );
    stats.insert(
        "summary_tokens_total".to_owned(),
        json!(response.total_tokens),
    );
    stats.insert("summary_cost_usd".to_owned(), json!(response.cost_usd));
    if invalid_json {
        stats.insert("llm_invalid_json_total".to_owned(), json!(1));
    }
    Value::Object(stats)
}

fn inferred_usage_stats(response: &LlmResponse, invalid_json: bool) -> Value {
    let mut stats = serde_json::Map::new();
    stats.insert("inferred_dispatch_misses_total".to_owned(), json!(1));
    stats.insert(
        "inferred_tokens_input".to_owned(),
        json!(response.input_tokens),
    );
    stats.insert(
        "inferred_tokens_cached_input".to_owned(),
        json!(response.cached_input_tokens),
    );
    stats.insert(
        "inferred_tokens_output".to_owned(),
        json!(response.output_tokens),
    );
    stats.insert(
        "inferred_tokens_total".to_owned(),
        json!(response.total_tokens),
    );
    stats.insert("inferred_cost_usd".to_owned(), json!(response.cost_usd));
    if invalid_json {
        stats.insert("llm_invalid_json_total".to_owned(), json!(1));
    }
    Value::Object(stats)
}

fn token_ceiling_envelope(message: &str) -> Value {
    json!({
        "ok": false,
        "result": null,
        "error": {
            "code": "token-ceiling-exceeded",
            "message": message,
            "retryable": false
        },
        "diagnostics": [
            {
                "code": "CLA-LLM-TOKEN-CEILING-EXCEEDED",
                "message": message
            }
        ],
        "truncated": false,
        "truncation_reason": null,
        "stats_delta": {
            "token_ceiling_exceeded_total": 1
        }
    })
}

fn issues_unavailable(filigree_endpoint: &Value, reason: &str, message: &str) -> Value {
    success_envelope(json!({
        "available": false,
        "result_kind": "unavailable",
        "filigree_endpoint": filigree_endpoint,
        "reason": reason,
        "message": message,
        "matched": [],
        "drifted": [],
        "not_found": []
    }))
}

fn association_json(
    association: &EntityAssociation,
    entity: Option<&EntityRow>,
    current_content_hash: Option<&str>,
    drift_status: &str,
) -> Value {
    json!({
        "issue_id": association.issue_id,
        "entity_id": association.clarion_entity_id,
        "entity": entity.map(entity_json),
        "content_hash_at_attach": association.content_hash_at_attach,
        "current_content_hash": current_content_hash,
        "attached_at": association.attached_at,
        "attached_by": association.attached_by,
        "drift_status": drift_status
    })
}

fn summary_read_error(read: SummaryRead) -> Value {
    match read {
        SummaryRead::EntityNotFound(id) => tool_error_envelope(
            "entity-not-found",
            &format!("entity {id} was not found"),
            false,
        ),
        SummaryRead::MissingContentHash(id) => tool_error_envelope(
            "content-hash-missing",
            &format!("entity {id} has no content hash for summary cache keying"),
            false,
        ),
        SummaryRead::ScopeDeferred(entity) => summary_scope_deferred(&entity),
        SummaryRead::BriefingBlocked(entity, reason) => summary_briefing_blocked(&entity, &reason),
        SummaryRead::Ready(_) => unreachable!("ready summary read is not an error"),
    }
}

#[derive(Debug)]
struct SourceExcerptError {
    entity_id: String,
    stored_content_hash: String,
    current_content_hash: String,
}

impl SourceExcerptError {
    fn message(&self) -> String {
        format!(
            "entity {} source content drifted: stored content_hash {} but current file hashes to {}; rerun `clarion analyze` before requesting LLM output",
            self.entity_id, self.stored_content_hash, self.current_content_hash
        )
    }

    fn to_envelope(&self) -> Value {
        tool_error_envelope("content-drift", &self.message(), false)
    }

    fn to_inferred_failure(&self) -> InferredDispatchFailure {
        InferredDispatchFailure::new("content-drift", &self.message(), false)
    }
}

/// Deterministic structural summary used when the LLM returns non-JSON
/// (clarion-ed246ca3aa). Carries the entity's identity plus a bounded head of
/// its own source (where the signature and docstring live), so the caller gets
/// usable orientation instead of a billed error. `kind: structural-fallback`
/// lets consumers distinguish it from a real LLM summary.
fn structural_summary_json(entity: &EntityRow, source_excerpt: &str) -> String {
    const STRUCTURAL_SUMMARY_MAX_CHARS: usize = 1200;
    let mut source_head: String = source_excerpt
        .chars()
        .take(STRUCTURAL_SUMMARY_MAX_CHARS)
        .collect();
    let source_truncated = source_excerpt.chars().count() > STRUCTURAL_SUMMARY_MAX_CHARS;
    if source_truncated {
        source_head.push('…');
    }
    serde_json::to_string(&json!({
        "kind": "structural-fallback",
        "note": "LLM summary unavailable (provider returned non-JSON); deterministic structural summary derived from the entity source.",
        "entity_kind": entity.kind,
        "short_name": entity.short_name,
        "qualified_name": entity.name,
        "source_head": source_head,
        "source_truncated": source_truncated
    }))
    .expect("structural summary serializes")
}

fn summary_success_envelope(
    entity: &EntityRow,
    entry: &SummaryCacheEntry,
    cache_hit: bool,
    stale_semantic: bool,
    cached_input_tokens: Option<i64>,
    stats_delta: Value,
) -> Value {
    let summary = serde_json::from_str::<Value>(&entry.summary_json).unwrap_or_else(|_| {
        json!({
            "raw": entry.summary_json
        })
    });
    let mut usage = serde_json::Map::new();
    usage.insert("tokens_input".to_owned(), json!(entry.tokens_input));
    if let Some(tokens) = cached_input_tokens {
        usage.insert("tokens_cached_input".to_owned(), json!(tokens));
    }
    usage.insert("tokens_output".to_owned(), json!(entry.tokens_output));
    usage.insert(
        "tokens_total".to_owned(),
        json!(entry.tokens_input + entry.tokens_output),
    );
    success_envelope_with_stats(
        json!({
            "available": true,
            "entity": entity_json(entity),
            "summary": summary,
            "cache": {
                "hit": cache_hit,
                "prompt_template_id": entry.key.prompt_template_id,
                "model_id": entry.key.model_tier,
                "guidance_fingerprint": entry.key.guidance_fingerprint,
                "stale_semantic": stale_semantic,
                "created_at": entry.created_at,
                "last_accessed_at": entry.last_accessed_at
            },
            "usage": Value::Object(usage)
        }),
        stats_delta,
    )
}

fn summary_scope_deferred(entity: &EntityRow) -> Value {
    success_envelope(json!({
        "available": false,
        "reason": "summary-scope-deferred",
        "message": "subsystem summaries are deferred to v0.2",
        "entity": entity_json(entity)
    }))
}

fn summary_briefing_blocked(entity: &EntityRow, reason: &str) -> Value {
    let remediation = if reason == "unscanned_source" {
        "Entity source file was not covered by the pre-ingest secret scan. Re-run with scanner coverage for that path or fix the plugin source path before requesting a summary."
    } else {
        "File flagged by pre-ingest secret scan. Fix the secret or whitelist via .clarion/secrets-baseline.yaml. See ADR-013."
    };
    success_envelope(json!({
        "available": false,
        "entity_id": entity.id,
        "entity": entity_json(entity),
        "summary": null,
        "briefing_blocked": reason,
        "remediation": remediation
    }))
}

fn briefing_block_reason(entity: &EntityRow) -> Option<String> {
    entity_properties_json(entity)
        .get("briefing_blocked")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn tool_json_rpc_response(id: &Value, envelope: &Value) -> Value {
    let is_error = !envelope
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or_default();
    result_response(
        id,
        &json!({
            "content": [
                {
                    "type": "text",
                    "text": serde_json::to_string(&envelope).expect("tool envelope serializes")
                }
            ],
            "isError": is_error
        }),
    )
}

fn entity_json(entity: &EntityRow) -> Value {
    json!({
        "id": entity.id,
        "kind": entity.kind,
        "name": entity.name,
        "short_name": entity.short_name,
        "source_file_path": entity.source_file_path,
        "source_line_start": entity.source_line_start,
        "source_line_end": entity.source_line_end,
        "content_hash": entity.content_hash
    })
}

fn entity_properties_json(entity: &EntityRow) -> Value {
    serde_json::from_str::<Value>(&entity.properties_json)
        .expect("entity properties_json should be valid JSON")
}

/// Maximum number of ambiguity alternatives `entity_at` reports. Genuine
/// same-granularity overlaps are rare; this only bounds a pathological file.
const ENTITY_CONTEXT_MAX_ALTERNATIVES: usize = 8;

/// The decorator/declaration/body sub-ranges a plugin records for a
/// function/class entity in `properties_json.definition` (clarion-460def6a51).
/// All optional: older indexes, modules, and plugins that don't emit the block
/// leave every field `None`.
struct DefinitionSpan {
    decl_line: Option<i64>,
    body_line_start: Option<i64>,
    decorator_line_start: Option<i64>,
    decorator_line_end: Option<i64>,
}

impl DefinitionSpan {
    fn from_entity(entity: &EntityRow) -> Self {
        let def = serde_json::from_str::<Value>(&entity.properties_json)
            .ok()
            .and_then(|props| props.get("definition").cloned());
        let get = |key: &str| -> Option<i64> {
            def.as_ref()
                .and_then(|d| d.get(key))
                .and_then(Value::as_i64)
        };
        Self {
            decl_line: get("decl_line"),
            body_line_start: get("body_line_start"),
            decorator_line_start: get("decorator_line_start"),
            decorator_line_end: get("decorator_line_end"),
        }
    }
}

/// Classify *why* `line` resolved to `entity`: a decorator line, the
/// declaration/signature, the body, or merely a containing scope (module, or
/// an entity without recorded sub-ranges). Honest by construction — a blank or
/// comment line that only the module spans reports `containing_range`, never a
/// fabricated exact match (clarion-460def6a51 acceptance #3).
fn match_reason_for(line: i64, entity: &EntityRow) -> &'static str {
    if entity.kind == "module" {
        return "containing_range";
    }
    let def = DefinitionSpan::from_entity(entity);
    let Some(decl_line) = def.decl_line else {
        return "containing_range";
    };
    if let Some(decorator_start) = def.decorator_line_start
        && line >= decorator_start
        && line < decl_line
    {
        return "decorator_range";
    }
    if let Some(body_line_start) = def.body_line_start {
        if line >= body_line_start {
            return "body_range";
        }
        return "declaration";
    }
    if line == decl_line {
        return "declaration";
    }
    "containing_range"
}

/// Span length in lines used to detect same-granularity ambiguity. `None` when
/// either bound is missing.
fn span_len(entity: &EntityRow) -> Option<i64> {
    Some(entity.source_line_end? - entity.source_line_start?)
}

/// Compact entity descriptor for the containing stack — enough to orient
/// without the full `entity_json` payload.
fn stack_entity_json(entity: &EntityRow) -> Value {
    json!({
        "id": entity.id,
        "kind": entity.kind,
        "short_name": entity.short_name,
        "name": entity.name,
        "source_line_start": entity.source_line_start,
        "source_line_end": entity.source_line_end,
    })
}

/// Build the additive `entity_context` evidence block for `entity_at`
/// (clarion-460def6a51): the match reason, the module→entity containing stack,
/// the matched entity's sub-ranges, same-granularity ambiguity alternatives,
/// and index freshness. Returns a `no_match` shell when no entity spans the
/// line (e.g. an unindexed file).
fn entity_context_json(
    line: Option<i64>,
    matched: Option<&EntityRow>,
    candidates: &[EntityRow],
    ancestors: &[EntityRow],
    snapshot: &crate::snapshot::ProjectSnapshot,
) -> Value {
    let freshness = json!({
        "staleness": snapshot.staleness(),
        "last_analyzed_at": snapshot.last_analyzed_at(),
        "degraded": snapshot.degraded(),
    });

    let Some(matched) = matched else {
        return json!({
            "query_line": line,
            "match_reason": "no_match",
            "containing_stack": [],
            "ranges": Value::Null,
            "alternatives": [],
            "freshness": freshness,
        });
    };

    // Containing stack outermost (module) → matched entity, inclusive.
    let mut containing_stack: Vec<Value> = ancestors.iter().rev().map(stack_entity_json).collect();
    containing_stack.push(stack_entity_json(matched));

    let def = DefinitionSpan::from_entity(matched);
    let ranges = json!({
        "source_line_start": matched.source_line_start,
        "source_line_end": matched.source_line_end,
        "decl_line": def.decl_line,
        "body_line_start": def.body_line_start,
        "decorator_line_start": def.decorator_line_start,
        "decorator_line_end": def.decorator_line_end,
    });

    // Ambiguity: other candidates sharing the winner's span length are genuine
    // same-granularity overlaps. Strictly larger spans are the nesting stack
    // already captured above, so they are not alternatives. Only meaningful for
    // a line query; an `entity`-id lookup has no line to disambiguate.
    let matched_len = span_len(matched);
    let alternatives: Vec<Value> = match line {
        Some(line) => candidates
            .iter()
            .skip(1)
            .filter(|cand| matched_len.is_some() && span_len(cand) == matched_len)
            .take(ENTITY_CONTEXT_MAX_ALTERNATIVES)
            .map(|cand| {
                json!({
                    "entity": entity_json(cand),
                    "match_reason": match_reason_for(line, cand),
                })
            })
            .collect(),
        None => Vec::new(),
    };

    // For a line query, explain why that line matched; for a direct `entity`-id
    // lookup there is no line, so the reason is simply "entity".
    let match_reason = match line {
        Some(line) => match_reason_for(line, matched),
        None => "entity",
    };

    json!({
        "query_line": line,
        "match_reason": match_reason,
        "containing_stack": containing_stack,
        "ranges": ranges,
        "alternatives": alternatives,
        "freshness": freshness,
    })
}

/// Per-section neighbor cap for `orientation_pack` — keeps the packet bounded
/// while still surfacing the most relevant edges; overflow is reported in
/// `omitted`.
const ORIENTATION_PACK_MAX_NEIGHBORS: usize = 10;

/// Call-graph traversal depth for `orientation_pack`'s compact execution paths.
/// Matches the `execution_paths_from` default so the packet's paths line up
/// with a follow-up call to that tool.
const ORIENTATION_PACK_PATH_DEPTH: usize = 3;

/// Sync portion of an `orientation_pack`: everything one reader snapshot can
/// produce, plus the flags the async assembly stage needs for warnings and the
/// `omitted` block. Issues + health are layered on afterward.
struct OrientationCore {
    primary_id: Option<String>,
    primary_kind: Option<String>,
    /// True when the request used the `entity`-id form (so a `None`
    /// `primary_id` is a hard not-found, not a graceful `no_match`).
    lookup_was_id: bool,
    packet: Value,
    freshness: Value,
    staleness_stale: bool,
    neighbors_omitted: serde_json::Map<String, Value>,
    paths_truncation_reason: Option<String>,
}

/// Sort a neighbor list by entity id (stable, deterministic) and cap it,
/// returning the kept list and how many were dropped.
fn cap_neighbor_list(mut list: Vec<Value>, cap: usize) -> (Vec<Value>, usize) {
    list.sort_by(|a, b| neighbor_sort_key(a).cmp(neighbor_sort_key(b)));
    let omitted = list.len().saturating_sub(cap);
    list.truncate(cap);
    (list, omitted)
}

/// Entity id of a neighbor JSON record (`{"entity": {"id": ...}, ...}` or a
/// bare entity object), used as a deterministic sort key.
fn neighbor_sort_key(value: &Value) -> &str {
    value
        .get("entity")
        .and_then(|entity| entity.get("id"))
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// Deterministic follow-up reads for an `orientation_pack`. Suggests the full
/// source, a pre-spend cost preview, the owning subsystem, and a drill-down
/// into the first callee (by sorted id) when one exists. Empty when there is no
/// primary entity.
fn orientation_suggested_reads(
    packet: &Value,
    primary_id: Option<&str>,
    primary_kind: Option<&str>,
) -> Vec<Value> {
    let Some(primary_id) = primary_id else {
        return Vec::new();
    };
    let mut reads = vec![
        json!({
            "tool": "source_for_entity",
            "args": {"id": primary_id},
            "why": "read the entity's source with line numbers",
        }),
        json!({
            "tool": "summary_preview_cost",
            "args": {"id": primary_id},
            "why": "estimate the cost of an LLM briefing before spending",
        }),
    ];
    // A subsystem's useful drill-down is its members; for any other kind it is
    // the owning subsystem.
    if primary_kind == Some("subsystem") {
        reads.push(json!({
            "tool": "subsystem_members",
            "args": {"id": primary_id},
            "why": "list the entities clustered into this subsystem",
        }));
    } else {
        reads.push(json!({
            "tool": "subsystem_of",
            "args": {"id": primary_id},
            "why": "see which subsystem this entity belongs to",
        }));
    }
    // Drill into the first callee (lists are already id-sorted), if any.
    if let Some(callee_id) = packet
        .get("neighbors")
        .and_then(|neighbors| neighbors.get("callees"))
        .and_then(Value::as_array)
        .and_then(|callees| callees.first())
        .and_then(|callee| callee.get("entity"))
        .and_then(|entity| entity.get("id"))
        .and_then(Value::as_str)
    {
        reads.push(json!({
            "tool": "orientation_pack",
            "args": {"entity": callee_id},
            "why": "orient on the primary callee",
        }));
    }
    reads
}

/// How recently the analyze progress file must have been stamped for
/// `analyze_status` to call progress "observed" rather than possibly stalled.
const ANALYZE_HEARTBEAT_STALE_SECS: i64 = 30;

/// Live state of a registry-tracked analyze run, captured under the lock.
enum LiveRun {
    Alive {
        started_at: String,
        progress_path: PathBuf,
    },
    Exited {
        started_at: String,
        cancelled: bool,
    },
    Absent,
}

/// Result of an `analyze_cancel` attempt against the registry.
enum CancelOutcome {
    Cancelled,
    AlreadyExited { cancelled: bool },
    Absent,
}

/// Read and parse the analyze progress snapshot, if present and valid JSON.
fn read_progress_snapshot(path: &std::path::Path) -> Option<Value> {
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

/// Parse a timestamp to Unix seconds, accepting both the MCP clock's
/// `unix:<seconds>` form and the RFC3339 form analyze writes into the progress
/// file's `heartbeat_at`. `None` if neither parses.
fn parse_to_unix_seconds(value: &str) -> Option<i64> {
    use time::OffsetDateTime;
    use time::format_description::well_known::Rfc3339;
    if let Some(rest) = value.strip_prefix("unix:") {
        return rest.trim().parse().ok();
    }
    OffsetDateTime::parse(value, &Rfc3339)
        .ok()
        .map(OffsetDateTime::unix_timestamp)
}

/// Whole seconds between two timestamps (`now - start`), or `None` if either
/// fails to parse. Accepts mixed `unix:`/RFC3339 forms.
fn elapsed_seconds(start: &str, now: &str) -> Option<i64> {
    Some(parse_to_unix_seconds(now)? - parse_to_unix_seconds(start)?)
}

/// Whether the heartbeat is recent enough to treat the run as actively
/// progressing (heartbeat age within `max_age_secs`).
fn progress_observed(heartbeat: &str, now: &str, max_age_secs: i64) -> bool {
    elapsed_seconds(heartbeat, now).is_some_and(|age| (0..=max_age_secs).contains(&age))
}

/// True when a run's `stats` JSON marks it as cancelled (vs an ordinary
/// failure) — the MCP writes `terminal_reason="cancelled"` on cancel.
fn run_stats_is_cancelled(stats_json: &str) -> bool {
    serde_json::from_str::<Value>(stats_json)
        .ok()
        .and_then(|stats| {
            stats
                .get("terminal_reason")
                .and_then(Value::as_str)
                .map(|reason| reason == "cancelled")
        })
        .unwrap_or(false)
}

/// Map a `runs.status` value to the `analyze_status` vocabulary. A row still
/// reading `running` after the process exited is abnormal (the process died
/// without finalizing) and reported as `failed`.
fn map_run_status(db_status: &str, stats_json: &str) -> &'static str {
    match db_status {
        "completed" => "completed",
        "skipped_no_plugins" => "skipped_no_plugins",
        "failed" if run_stats_is_cancelled(stats_json) => "cancelled",
        _ => "failed",
    }
}

/// Maximum number of (span + context) lines `source_for_entity` will emit
/// before truncating, so a pathologically large entity never floods an agent's
/// context. The span itself is bounded by the entity; this caps the total.
const SOURCE_FOR_ENTITY_MAX_LINES: usize = 2_000;

/// Direction for `call_sites`: outgoing (this entity is the caller) or incoming
/// (this entity is the callee).
#[derive(Clone, Copy)]
enum CallSiteRole {
    Caller,
    Callee,
}

/// Edge-kind filter for `call_sites`.
#[derive(Clone, Copy)]
enum CallSiteKind {
    Both,
    Calls,
    References,
}

impl CallSiteKind {
    fn includes_calls(self) -> bool {
        matches!(self, Self::Both | Self::Calls)
    }
    fn includes_references(self) -> bool {
        matches!(self, Self::Both | Self::References)
    }
}

/// Production/test path scope for `call_sites`. Best-effort: source-file
/// production/test partitioning is not indexed, so this is a heuristic over the
/// file path (documented as such in the tool description).
#[derive(Clone, Copy)]
enum PathScope {
    All,
    Production,
    Test,
}

impl PathScope {
    fn admits(self, path: Option<&str>) -> bool {
        match (self, path) {
            (Self::All, _) => true,
            // A site whose owning file can't be resolved is excluded from a
            // narrowed scope rather than guessed into it.
            (_, None) => false,
            (Self::Test, Some(p)) => is_test_path(p),
            (Self::Production, Some(p)) => !is_test_path(p),
        }
    }
}

/// Conventional Python test-path heuristic (pytest/unittest layouts). Not a
/// substitute for indexed metadata — see `PathScope`.
fn is_test_path(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(lower.as_str());
    lower.contains("/tests/")
        || lower.contains("/test/")
        || file.starts_with("test_")
        || file.ends_with("_test.py")
        || file == "conftest.py"
}

/// Per-call cap on each of the resolved and unresolved site lists.
const CALL_SITES_MAX: usize = 200;

/// 1-based line and 0-based byte column for a byte offset into `content`.
// The slices are a single source file; a dependency on `bytecount` for the
// newline count is not warranted here.
#[allow(clippy::naive_bytecount)]
fn byte_line_col(content: &str, byte_offset: i64) -> Option<(i64, i64)> {
    let off = usize::try_from(byte_offset).ok()?;
    let bytes = content.as_bytes();
    if off > bytes.len() {
        return None;
    }
    let line = bytes[..off].iter().filter(|&&b| b == b'\n').count() + 1;
    let line_start = bytes[..off]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1);
    Some((
        i64::try_from(line).ok()?,
        i64::try_from(off - line_start).ok()?,
    ))
}

/// The text of `line` (1-based) in `content`, or empty if out of range.
fn line_text_at(content: &str, line: i64) -> String {
    let idx = usize::try_from(line - 1).unwrap_or(usize::MAX);
    content.lines().nth(idx).unwrap_or("").to_owned()
}

/// One resolved call/reference site before file resolution.
struct ResolvedSite {
    owner_id: String,
    edge_kind: &'static str,
    other_id: String,
    confidence: EdgeConfidence,
    byte_start: Option<i64>,
    byte_end: Option<i64>,
}

/// One static call Clarion could not bind (kept separate from resolved sites).
struct UnboundSite {
    owner_id: String,
    callee_expr: String,
    byte_start: i64,
    byte_end: i64,
}

/// Gather the resolved and unbound (statically-unbindable) call/reference sites
/// for `entity` in the requested direction, applying the edge-kind and
/// confidence filters. File/line resolution happens in [`build_call_sites`].
fn collect_call_sites(
    conn: &rusqlite::Connection,
    entity: &EntityRow,
    role: CallSiteRole,
    kind: CallSiteKind,
    confidence: EdgeConfidence,
) -> Result<(Vec<ResolvedSite>, Vec<UnboundSite>), StorageError> {
    let mut resolved: Vec<ResolvedSite> = Vec::new();
    let mut unbound: Vec<UnboundSite> = Vec::new();

    match role {
        CallSiteRole::Caller => {
            if kind.includes_calls() {
                for edge in call_edges_from(conn, &entity.id, confidence)? {
                    resolved.push(ResolvedSite {
                        owner_id: entity.id.clone(),
                        edge_kind: "calls",
                        other_id: edge.to_id,
                        confidence: edge.confidence,
                        byte_start: edge.source_byte_start,
                        byte_end: edge.source_byte_end,
                    });
                }
                for site in unresolved_call_sites_for_caller(conn, &entity.id, CALL_SITES_MAX)? {
                    unbound.push(UnboundSite {
                        owner_id: entity.id.clone(),
                        callee_expr: site.callee_expr,
                        byte_start: site.source_byte_start,
                        byte_end: site.source_byte_end,
                    });
                }
            }
            if kind.includes_references() {
                for r in reference_edges_for_entity(conn, &entity.id, ReferenceDirection::Out)? {
                    if r.confidence <= confidence {
                        resolved.push(ResolvedSite {
                            owner_id: entity.id.clone(),
                            edge_kind: "references",
                            other_id: r.neighbor_id,
                            confidence: r.confidence,
                            byte_start: r.source_byte_start,
                            byte_end: r.source_byte_end,
                        });
                    }
                }
            }
        }
        CallSiteRole::Callee => {
            if kind.includes_calls() {
                for edge in call_edges_targeting(conn, &entity.id, confidence)? {
                    resolved.push(ResolvedSite {
                        owner_id: edge.from_id.clone(),
                        edge_kind: "calls",
                        other_id: edge.from_id,
                        confidence: edge.confidence,
                        byte_start: edge.source_byte_start,
                        byte_end: edge.source_byte_end,
                    });
                }
                for site in unresolved_callers_for_target(conn, entity, CALL_SITES_MAX)? {
                    unbound.push(UnboundSite {
                        owner_id: site.caller_entity_id,
                        callee_expr: site.callee_expr,
                        byte_start: site.source_byte_start,
                        byte_end: site.source_byte_end,
                    });
                }
            }
            if kind.includes_references() {
                for r in reference_edges_for_entity(conn, &entity.id, ReferenceDirection::In)? {
                    if r.confidence <= confidence {
                        resolved.push(ResolvedSite {
                            owner_id: r.neighbor_id.clone(),
                            edge_kind: "references",
                            other_id: r.neighbor_id,
                            confidence: r.confidence,
                            byte_start: r.source_byte_start,
                            byte_end: r.source_byte_end,
                        });
                    }
                }
            }
        }
    }

    Ok((resolved, unbound))
}

/// Build the `call_sites` payload. Returns `Ok(None)` when the entity does not
/// exist (so the caller can emit a not-found envelope).
fn build_call_sites(
    conn: &rusqlite::Connection,
    entity_id: &str,
    role: CallSiteRole,
    kind: CallSiteKind,
    confidence: EdgeConfidence,
    path: PathScope,
) -> Result<Option<Value>, StorageError> {
    let Some(entity) = entity_by_id(conn, entity_id)? else {
        return Ok(None);
    };

    let (resolved, unbound) = collect_call_sites(conn, &entity, role, kind, confidence)?;

    // Resolve each site's owning file once, mapping the byte anchor to a line.
    let mut owner_path: HashMap<String, Option<String>> = HashMap::new();
    let mut file_content: HashMap<String, Option<String>> = HashMap::new();
    // The queried entity's own path is known without a lookup.
    owner_path.insert(entity.id.clone(), entity.source_file_path.clone());

    let mut site_values = Vec::new();
    let mut truncated = false;
    for site in resolved {
        if site_values.len() >= CALL_SITES_MAX {
            truncated = true;
            break;
        }
        let path_str = resolve_owner_path(conn, &mut owner_path, &site.owner_id)?;
        if !path.admits(path_str.as_deref()) {
            continue;
        }
        let (line, column, line_text) =
            anchor_line(&mut file_content, path_str.as_deref(), site.byte_start);
        site_values.push(json!({
            "edge_kind": site.edge_kind,
            "other_id": site.other_id,
            "confidence": site.confidence.as_str(),
            "file": path_str,
            "line": line,
            "column": column,
            "line_text": line_text,
            "byte_start": site.byte_start,
            "byte_end": site.byte_end
        }));
    }

    let mut unresolved_values = Vec::new();
    for site in unbound {
        if unresolved_values.len() >= CALL_SITES_MAX {
            truncated = true;
            break;
        }
        let path_str = resolve_owner_path(conn, &mut owner_path, &site.owner_id)?;
        if !path.admits(path_str.as_deref()) {
            continue;
        }
        let (line, column, line_text) = anchor_line(
            &mut file_content,
            path_str.as_deref(),
            Some(site.byte_start),
        );
        unresolved_values.push(json!({
            "callee_expr": site.callee_expr,
            "file": path_str,
            "line": line,
            "column": column,
            "line_text": line_text,
            "byte_start": site.byte_start,
            "byte_end": site.byte_end
        }));
    }

    Ok(Some(json!({
        "entity": entity_json(&entity),
        "role": match role { CallSiteRole::Caller => "caller", CallSiteRole::Callee => "callee" },
        "filters": {
            "kind": match kind {
                CallSiteKind::Both => "both",
                CallSiteKind::Calls => "calls",
                CallSiteKind::References => "references",
            },
            "confidence": confidence.as_str(),
            "path": match path {
                PathScope::All => "all",
                PathScope::Production => "production",
                PathScope::Test => "test",
            }
        },
        "sites": site_values,
        "unresolved_sites": unresolved_values,
        "truncated": truncated,
        "scope_excludes": call_graph_scope_excludes(confidence)
    })))
}

/// Memoized lookup of an owner entity's source file path.
fn resolve_owner_path(
    conn: &rusqlite::Connection,
    cache: &mut HashMap<String, Option<String>>,
    owner_id: &str,
) -> Result<Option<String>, StorageError> {
    if let Some(path) = cache.get(owner_id) {
        return Ok(path.clone());
    }
    let path = entity_by_id(conn, owner_id)?.and_then(|e| e.source_file_path);
    cache.insert(owner_id.to_owned(), path.clone());
    Ok(path)
}

/// Map a byte anchor to (line, column, `line_text`), reading + caching the file.
/// Any piece that can't be resolved degrades to JSON null / empty rather than
/// failing the whole query.
fn anchor_line(
    file_content: &mut HashMap<String, Option<String>>,
    path: Option<&str>,
    byte_start: Option<i64>,
) -> (Value, Value, String) {
    let (Some(path), Some(byte_start)) = (path, byte_start) else {
        return (Value::Null, Value::Null, String::new());
    };
    let content = file_content
        .entry(path.to_owned())
        .or_insert_with(|| std::fs::read_to_string(path).ok());
    let Some(content) = content.as_deref() else {
        return (Value::Null, Value::Null, String::new());
    };
    match byte_line_col(content, byte_start) {
        Some((line, column)) => (json!(line), json!(column), line_text_at(content, line)),
        None => (Value::Null, Value::Null, String::new()),
    }
}

/// Build the `source_for_entity` payload: the entity's exact indexed line span
/// plus `context_lines` of surrounding context, line-numbered and drift-checked.
///
/// Returns an explicit `source_status` rather than a stale or misleading
/// snippet when the source cannot be trusted: `missing` (file gone),
/// `no_source_path` / `no_range` (no anchor to read), `binary` (non-UTF-8), or
/// `drifted` (the file no longer hashes to the indexed `content_hash`).
fn source_for_entity_json(entity: &EntityRow, context_lines: usize) -> Value {
    let identity = entity_json(entity);

    let Some(path) = entity.source_file_path.as_deref() else {
        return json!({"entity": identity, "source_status": "no_source_path"});
    };
    let (Some(start_line), Some(end_line)) = (entity.source_line_start, entity.source_line_end)
    else {
        return json!({
            "entity": identity,
            "source_file_path": path,
            "source_status": "no_range"
        });
    };

    let Ok(bytes) = std::fs::read(path) else {
        return json!({
            "entity": identity,
            "source_file_path": path,
            "source_status": "missing"
        });
    };
    let Ok(source) = String::from_utf8(bytes.clone()) else {
        return json!({
            "entity": identity,
            "source_file_path": path,
            "source_status": "binary"
        });
    };

    // Refuse to hand back a snippet that no longer matches what was indexed.
    if let (Some(stored), Some(current)) = (
        entity.content_hash.as_deref(),
        current_source_content_hash(entity, &bytes, Some(&source)),
    ) && stored != current
    {
        return json!({
            "entity": identity,
            "source_file_path": path,
            "source_status": "drifted",
            "drift": {
                "stored_content_hash": stored,
                "current_content_hash": current
            }
        });
    }

    let lines: Vec<&str> = source.lines().collect();
    let total = i64::try_from(lines.len()).unwrap_or(i64::MAX);
    // Clamp the span to the file, then widen by the context window. 1-based,
    // inclusive on both ends.
    let span_start = start_line.max(1);
    let span_end = end_line.min(total).max(span_start);
    let ctx = i64::try_from(context_lines).unwrap_or(i64::MAX);
    let window_start = (span_start - ctx).max(1);
    let window_end = (span_end + ctx).min(total);

    let mut emitted = Vec::new();
    let mut truncated = false;
    let mut number = window_start;
    while number <= window_end {
        if emitted.len() >= SOURCE_FOR_ENTITY_MAX_LINES {
            truncated = true;
            break;
        }
        let idx = usize::try_from(number - 1).unwrap_or(usize::MAX);
        let text = lines.get(idx).copied().unwrap_or("");
        emitted.push(json!({
            "number": number,
            "text": text,
            "in_entity": number >= span_start && number <= span_end
        }));
        number += 1;
    }

    json!({
        "entity": identity,
        "source_file_path": path,
        "source_status": "ok",
        "line_start": span_start,
        "line_end": span_end,
        "context_lines": context_lines,
        "window_start": window_start,
        "window_end": window_end,
        "lines": emitted,
        "truncated": truncated
    })
}

fn verified_source_excerpt(entity: &EntityRow) -> Result<String, SourceExcerptError> {
    let Some(path) = entity.source_file_path.as_deref() else {
        return Ok(String::new());
    };
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(String::new());
    };
    let source = String::from_utf8(bytes.clone()).ok();
    if let (Some(stored_content_hash), Some(current_content_hash)) = (
        entity.content_hash.as_deref(),
        current_source_content_hash(entity, &bytes, source.as_deref()),
    ) && stored_content_hash != current_content_hash
    {
        return Err(SourceExcerptError {
            entity_id: entity.id.clone(),
            stored_content_hash: stored_content_hash.to_owned(),
            current_content_hash,
        });
    }
    let Some(source) = source else {
        return Ok(String::new());
    };
    let excerpt = line_range_excerpt(&source, entity.source_line_start, entity.source_line_end)
        .unwrap_or(source);
    Ok(truncate_excerpt(excerpt))
}

fn current_source_content_hash(
    entity: &EntityRow,
    file_bytes: &[u8],
    source: Option<&str>,
) -> Option<String> {
    if entity.kind == "module" {
        return Some(blake3::hash(file_bytes).to_hex().to_string());
    }
    let source = source?;
    let start_line = entity.source_line_start?;
    let end_line = entity.source_line_end?;
    if start_line <= 0 || end_line < start_line {
        return None;
    }
    let start = usize::try_from(start_line - 1).ok()?;
    let mut end = usize::try_from(end_line).ok()?;
    let lines = source.lines().collect::<Vec<_>>();
    end = end.min(lines.len());
    if start >= end {
        return None;
    }
    let normalized = lines[start..end].join("\n");
    Some(blake3::hash(normalized.as_bytes()).to_hex().to_string())
}

fn line_range_excerpt(
    source: &str,
    start_line: Option<i64>,
    end_line: Option<i64>,
) -> Option<String> {
    let start_line = start_line?;
    let end_line = end_line?;
    if start_line <= 0 || end_line < start_line {
        return None;
    }
    let start = usize::try_from(start_line - 1).ok()?;
    let end = usize::try_from(end_line).ok()?;
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    let end = end.min(lines.len());
    if start >= end {
        return None;
    }
    Some(lines[start..end].concat())
}

fn truncate_excerpt(source: String) -> String {
    if source.len() > 8_000 {
        source.chars().take(8_000).collect()
    } else {
        source
    }
}

fn unresolved_sites_json(sites: &[UnresolvedCallSiteRow]) -> String {
    serde_json::to_string(
        &sites
            .iter()
            .map(|site| {
                json!({
                    "caller_entity_id": site.caller_entity_id,
                    "caller_content_hash": site.caller_content_hash,
                    "site_key": site.site_key,
                    "site_ordinal": site.site_ordinal,
                    "source_file_id": site.source_file_id,
                    "source_byte_start": site.source_byte_start,
                    "source_byte_end": site.source_byte_end,
                    "callee_expr": site.callee_expr
                })
            })
            .collect::<Vec<_>>(),
    )
    .expect("unresolved site JSON serializes")
}

fn entities_json(entities: &[EntityRow]) -> String {
    serde_json::to_string(&entities.iter().map(entity_json).collect::<Vec<_>>())
        .expect("candidate entity JSON serializes")
}

fn inferred_records_from_result(
    read: &InferredRead,
    result_json: &str,
    max_edges: usize,
) -> Result<Vec<InferredCallEdgeRecord>, InferredDispatchFailure> {
    let parsed: InferredCallsResponse = serde_json::from_str(result_json).map_err(|err| {
        InferredDispatchFailure::new(
            "llm-invalid-json",
            &format!("inferred provider returned invalid JSON: {err}"),
            true,
        )
    })?;
    let cache_key = inferred_edge_cache_key_id(&read.key);
    let sites_by_key = read
        .sites
        .iter()
        .map(|site| (site.site_key.as_str(), site))
        .collect::<HashMap<_, _>>();
    let mut records = Vec::new();
    for edge in parsed.edges.into_iter().take(max_edges) {
        if edge.target_id.trim().is_empty() {
            continue;
        }
        let site = match edge.site_key.as_deref() {
            Some(site_key) => sites_by_key.get(site_key).copied(),
            None if read.sites.len() == 1 => read.sites.first(),
            None => None,
        };
        let Some(site) = site else {
            continue;
        };
        let properties = json!({
            "model_id": read.key.model_id,
            "prompt_version": read.key.prompt_version,
            "caller_content_hash": read.key.caller_content_hash,
            "inference_cache_key": cache_key,
            "site_key": site.site_key,
            "model_confidence": edge.confidence,
            "rationale": edge.rationale
        });
        records.push(InferredCallEdgeRecord {
            from_id: read.caller.id.clone(),
            to_id: edge.target_id,
            source_file_id: site.source_file_id.clone(),
            source_byte_start: site.source_byte_start,
            source_byte_end: site.source_byte_end,
            properties_json: properties.to_string(),
        });
    }
    Ok(records)
}

fn stale_semantic(entry: &SummaryCacheEntry, caller_count: i64, fan_out: i64) -> bool {
    entry.stale_semantic
        || count_drifted(entry.caller_count, caller_count)
        || count_drifted(entry.fan_out, fan_out)
}

fn count_drifted(stored: i64, current: i64) -> bool {
    if stored == current {
        return false;
    }
    if stored == 0 {
        return current != 0;
    }
    i128::from((current - stored).abs()) * 2 > i128::from(stored.abs())
}

fn summary_cache_expired(created_at: &str, now: &str, max_age_days: u32) -> bool {
    let Some(created) = timestamp_day_index(created_at) else {
        return false;
    };
    let Some(current) = timestamp_day_index(now) else {
        return false;
    };
    current.saturating_sub(created) > i64::from(max_age_days)
}

fn timestamp_day_index(raw: &str) -> Option<i64> {
    if let Some(seconds) = raw.strip_prefix("unix:") {
        return seconds.parse::<i64>().ok().map(|value| value / 86_400);
    }
    let date = raw.get(..10)?;
    let date = Date::parse(date, format_description!("[year]-[month]-[day]")).ok()?;
    let unix_epoch = Date::from_calendar_date(1970, Month::January, 1)
        .expect("Unix epoch is a valid calendar date");
    Some(i64::from(date.to_julian_day() - unix_epoch.to_julian_day()))
}

/// The `max_output_tokens` ceiling a leaf summary request reserves. Reported by
/// `summary_preview_cost` as the output ceiling (not a length prediction).
const SUMMARY_MAX_OUTPUT_TOKENS: i64 = 512;

/// A provider-free, deterministic input-token estimate for `summary_preview_cost`:
/// roughly four characters per token. Intended only as a pre-spend order-of-
/// magnitude hint, not an exact count (the real count is recorded on the cache
/// row once a summary has actually run).
fn estimate_tokens_from_chars(text: &str) -> i64 {
    let chars = i64::try_from(text.chars().count()).unwrap_or(i64::MAX);
    // ceil(chars / 4) without the unstable i64::div_ceil.
    chars.saturating_add(3) / 4
}

fn default_now_string() -> String {
    let seconds = OffsetDateTime::now_utc().unix_timestamp();
    format!("unix:{seconds}")
}

fn caller_json(
    conn: &rusqlite::Connection,
    edge: &CallEdgeMatch,
) -> Result<Option<Value>, StorageError> {
    Ok(entity_by_id(conn, &edge.from_id)?.map(|entity| {
        json!({
            "entity": entity_json(&entity),
            "edge_confidence": edge.confidence.as_str(),
            "source_byte_start": edge.source_byte_start,
            "source_byte_end": edge.source_byte_end,
            "target_id": edge.to_id,
            "stored_to_id": edge.stored_to_id
        })
    }))
}

fn callee_json(
    conn: &rusqlite::Connection,
    edge: &CallEdgeMatch,
) -> Result<Option<Value>, StorageError> {
    Ok(entity_by_id(conn, &edge.to_id)?.map(|entity| {
        json!({
            "entity": entity_json(&entity),
            "edge_confidence": edge.confidence.as_str(),
            "source_byte_start": edge.source_byte_start,
            "source_byte_end": edge.source_byte_end,
            "stored_to_id": edge.stored_to_id
        })
    }))
}

/// Compacted execution-path payload: a deduplicated node table, id-only ranked
/// paths, and whether the path cap trimmed the ranked set.
struct CompactPaths {
    nodes: Vec<Value>,
    paths: Vec<Vec<String>>,
    path_cap_truncated: bool,
}

/// Compact, ranked execution-path payload (clarion-5b3eff9a91). Ranks paths
/// longest-first (deepest reachable flow) with a lexicographic tie-break for
/// determinism, applies the server path cap (clarion-23ae24358c), then emits a
/// deduplicated node table so each entity is serialized once instead of once per
/// path occurrence — the old per-path re-serialization produced responses that
/// blew the transport budget.
fn compact_execution_paths(
    conn: &rusqlite::Connection,
    mut paths: Vec<Vec<String>>,
    path_cap: usize,
) -> Result<CompactPaths, StorageError> {
    paths.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    let path_cap_truncated = paths.len() > path_cap;
    paths.truncate(path_cap);

    let mut node_ids: BTreeSet<String> = BTreeSet::new();
    for path in &paths {
        for id in path {
            node_ids.insert(id.clone());
        }
    }
    let nodes = node_ids
        .iter()
        .filter_map(|id| entity_by_id(conn, id).transpose())
        .map(|row| row.map(|entity| compact_node_json(&entity)))
        .collect::<Result<Vec<_>, StorageError>>()?;
    Ok(CompactPaths {
        nodes,
        paths,
        path_cap_truncated,
    })
}

/// Truncation reason for an execution-path response. `edge-cap` (traversal
/// stopped early, so the graph itself is incomplete) takes precedence over
/// `path-cap` (traversal finished but the ranked output was trimmed for size,
/// clarion-23ae24358c).
fn path_truncation_reason(edge_truncated: bool, path_cap_truncated: bool) -> Option<&'static str> {
    if edge_truncated {
        Some("edge-cap")
    } else if path_cap_truncated {
        Some("path-cap")
    } else {
        None
    }
}

/// A path node trimmed for token economy: identity + location only. The full id
/// already encodes the qualified name; `content_hash` and the redundant `name`
/// are dropped (clarion-5b3eff9a91).
fn compact_node_json(entity: &EntityRow) -> Value {
    json!({
        "id": entity.id,
        "kind": entity.kind,
        "short_name": entity.short_name,
        "source_file_path": entity.source_file_path,
        "source_line_start": entity.source_line_start,
        "source_line_end": entity.source_line_end
    })
}

fn reference_neighbors(
    conn: &rusqlite::Connection,
    entity_id: &str,
    direction: ReferenceDirection,
) -> Result<Vec<Value>, StorageError> {
    edge_neighbors_json(
        conn,
        reference_edges_for_entity(conn, entity_id, direction)?,
    )
}

/// `imports`-edge neighbors for a module entity (clarion-79d0ff6e14). Direction
/// `In` is the reverse-import lookup ("who imports this module").
fn import_neighbors(
    conn: &rusqlite::Connection,
    entity_id: &str,
    direction: ReferenceDirection,
) -> Result<Vec<Value>, StorageError> {
    edge_neighbors_json(conn, import_edges_for_entity(conn, entity_id, direction)?)
}

/// Reference neighbors for `neighborhood` / `orientation_pack`, rolled up to
/// module altitude when the entity is a module (clarion-79d0ff6e14).
///
/// References are tracked symbol-to-symbol, so a module's OWN reference edges
/// are almost always empty — "who imports this module / contract?" used to
/// answer `[]`. For a module we instead aggregate the `references` edges of
/// every transitively contained symbol (excluding intra-module wiring) and tag
/// each neighbor with the contained `via` symbol it touches. For any other
/// kind the direct symbol-level edges are returned unchanged (no `via`).
///
/// Returns `(neighbors, rolled_up)`; `rolled_up` is true only for modules.
fn reference_neighbors_for(
    conn: &rusqlite::Connection,
    entity_id: &str,
    entity_kind: &str,
    direction: ReferenceDirection,
) -> Result<(Vec<Value>, bool), StorageError> {
    if entity_kind == "module" {
        let edges = module_reference_rollup(conn, entity_id, direction)?;
        Ok((rolled_up_neighbors_json(conn, edges)?, true))
    } else {
        Ok((reference_neighbors(conn, entity_id, direction)?, false))
    }
}

fn rolled_up_neighbors_json(
    conn: &rusqlite::Connection,
    edges: Vec<RolledUpReferenceEdge>,
) -> Result<Vec<Value>, StorageError> {
    let mut neighbors = Vec::new();
    for edge in edges {
        if let Some(entity) = entity_by_id(conn, &edge.neighbor_id)? {
            let via = entity_by_id(conn, &edge.via_id)?;
            neighbors.push(json!({
                "entity": entity_json(&entity),
                "edge_confidence": edge.confidence.as_str(),
                "source_byte_start": edge.source_byte_start,
                "source_byte_end": edge.source_byte_end,
                // The module-contained symbol this edge actually touches, so a
                // rolled-up "who imports this module" answer names the importer
                // (entity) AND the imported symbol (via).
                "via": via.as_ref().map(entity_json),
            }));
        }
    }
    Ok(neighbors)
}

fn edge_neighbors_json(
    conn: &rusqlite::Connection,
    edges: Vec<ReferenceEdgeMatch>,
) -> Result<Vec<Value>, StorageError> {
    let mut neighbors = Vec::new();
    for edge in edges {
        if let Some(entity) = entity_by_id(conn, &edge.neighbor_id)? {
            neighbors.push(json!({
                "entity": entity_json(&entity),
                "edge_confidence": edge.confidence.as_str(),
                "source_byte_start": edge.source_byte_start,
                "source_byte_end": edge.source_byte_end
            }));
        }
    }
    Ok(neighbors)
}

fn result_response(id: &Value, result: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use clarion_core::{CachingModel, LlmProvider, LlmProviderError, LlmRequest, LlmResponse};
    use clarion_storage::{
        EntityRow, InferredEdgeCacheKey, ReaderPool, UnresolvedCallSiteRow, pragma, schema,
    };
    use rusqlite::Connection;
    use tokio::sync::mpsc;

    use super::{InferenceLlmState, InferredRead, ServerState, config::LlmConfig, list_tools};

    #[test]
    fn tools_list_exposes_exact_docstrings() {
        let tools = list_tools();

        assert_eq!(tools.len(), 18);
        assert_eq!(tools[0].name, "entity_at");
        assert_eq!(
            tools[0].description,
            "Return the innermost Clarion entity whose source range contains a file and line, plus an `entity_context` evidence block: match_reason (decorator_range / declaration / body_range / containing_range / no_match) explaining why the line matched, the module→entity containing stack, the matched entity's decl/body/decorator sub-ranges, any same-granularity ambiguity alternatives, and index freshness. Paths are normalized relative to the project root. A blank or comment line that only a module spans reports containing_range — never a fabricated exact match."
        );
        assert_eq!(tools[1].name, "find_entity");
        assert_eq!(
            tools[1].description,
            "Search Clarion entities by id, name, short name, and summary text stored on entity rows. Results are paginated and ranked by FTS match where possible. This does not traverse the graph and does not search on-demand summary_cache entries. Pass an optional `kind` (e.g. \"subsystem\", \"function\", \"class\", \"module\") to return only entities of that kind — the way to locate a subsystem without visually filtering results."
        );
        assert_eq!(tools[2].name, "callers_of");
        assert_eq!(
            tools[2].description,
            "Return entities that call the given entity. Default confidence is resolved, so ambiguous static candidates and LLM-inferred edges are excluded unless explicitly requested. Ambiguous edges expand all candidates; inferred edges may trigger bounded LLM dispatch. The result carries scope_excludes naming static blind spots not searched (e.g. attribute-receiver-calls) so an empty callers list is never read as a guaranteed true negative."
        );
        assert_eq!(tools[3].name, "execution_paths_from");
        assert_eq!(
            tools[3].description,
            "Return bounded calls-only execution paths starting at an entity. Default confidence is resolved. max_depth defaults to 3. Results are compact: a deduplicated nodes table plus paths as arrays of node ids (under a root), ranked longest-first. Traversal stops at the server edge cap and the response is capped at a maximum number of ranked paths; truncated/truncation_reason report edge-cap or path-cap when either trims. The result carries scope_excludes naming static blind spots not searched (e.g. attribute-receiver-calls)."
        );
        assert_eq!(tools[4].name, "summary");
        assert_eq!(
            tools[4].description,
            "Return an on-demand cached summary for one entity. In v0.1 this is leaf scope only: module summaries describe the module docstring and top-level members, not an aggregation of contained function/class summaries. If the LLM returns non-JSON the response degrades to a deterministic structural summary (kind: structural-fallback) built from the entity source, and that fallback is cached so a retry is a free cache hit rather than a re-billed failure."
        );
        assert_eq!(tools[5].name, "issues_for");
        assert_eq!(
            tools[5].description,
            "Return Filigree issues attached to this Clarion entity, optionally including issues attached to contained entities. Filigree is an enrichment source; if unavailable, the tool returns an unavailable envelope instead of failing Clarion. The result carries a result_kind (matched | no_matches | unavailable) so a reachable-but-empty Filigree is distinct from an unreachable one, and a filigree_endpoint block (configured vs resolved URL + resolution_source) so you can see which endpoint — e.g. a live ethereal port — the answer came from."
        );
        assert_eq!(tools[6].name, "neighborhood");
        assert_eq!(
            tools[6].description,
            "Return the one-hop Clarion neighborhood around an entity: callers, callees, container, contained entities, references, and imports (imports_in = who imports this module, imports_out = what it imports; module-to-module). Default confidence is resolved; ambiguous and inferred calls are opt-in. References and imports are not execution flow. When the entity is a module, references_in/references_out are rolled up over the symbols it contains (references_rolled_up=true) — each neighbor carries a `via` naming the contained symbol the edge touches, so \"who imports this module/contract\" is answered at module altitude rather than reading empty. The result carries scope_excludes naming blind spots not searched (e.g. attribute-receiver-calls) so empty sections are never read as guaranteed true negatives."
        );
        assert_eq!(tools[7].name, "subsystem_members");
        assert_eq!(
            tools[7].description,
            "List module entities assigned to a subsystem entity."
        );
        assert_eq!(tools[8].name, "subsystem_of");
        assert_eq!(
            tools[8].description,
            "Return the subsystem an entity belongs to — the reverse of subsystem_members. Accepts any entity id: a module resolves directly, while a function/class resolves through its nearest containing module. Returns the subsystem id/name and the module the membership was resolved through, or a no-subsystem result when the entity has no subsystem-assigned module ancestor."
        );
        assert_eq!(tools[9].name, "project_status");
        assert_eq!(
            tools[9].description,
            "Return deterministic Clarion diagnostics: repo root, db path, latest run (id/status/started/completed), entity/subsystem/edge/finding/briefing-blocked counts, index staleness, per-plugin entity counts from the current index, LLM policy (provider/live/cache), and the resolved Filigree endpoint (configured vs resolved URL + resolution source). Answers \"is the graph fresh, plugin-less, LLM-live, Filigree-reachable?\" without shelling out. No LLM call."
        );
        assert_eq!(tools[10].name, "summary_preview_cost");
        assert_eq!(
            tools[10].description,
            "Preview what calling summary(id) would cost BEFORE spending. Reports cache_status (hit | expired | miss), the cached row's real tokens/cost/age on a hit, an input-token estimate on a miss, the configured model, the LLM policy (provider/live/allow_live_provider/cache horizon), and live_spend_would_occur — true only when no fresh cache row exists AND a live provider is wired. A disabled/unconfigured LLM is reported distinctly from a cache miss. Never invokes the LLM provider."
        );
        assert_eq!(tools[11].name, "source_for_entity");
        assert_eq!(
            tools[11].description,
            "Return the exact indexed source span for one entity (its source_line_start..source_line_end, which includes any decorators/signature/docstring the plugin captured) plus a bounded window of surrounding context, as line-numbered lines each flagged in_entity true/false. No LLM call. Lets an agent read and trust the entity without shelling out. source_status reports `ok`, or — instead of a misleading stale snippet — `missing` (file gone), `no_range`/`no_source_path` (entity has no anchor), `binary` (non-UTF-8), or `drifted` (the file no longer matches the indexed content_hash; rerun `clarion analyze`). context_lines defaults to 10."
        );
        assert_eq!(tools[12].name, "call_sites");
        assert_eq!(
            tools[12].description,
            "Show the actual source sites behind calls/references edges, so an agent can see WHY Clarion believes an edge exists rather than trusting it blind. role=caller (default) returns this entity's outgoing sites (what it calls/references); role=callee returns incoming sites (who calls/references it). Each site carries the file path, 1-based line, byte column, the source line text, edge kind, confidence, and a resolution of resolved | ambiguous (with candidate ids) | unresolved (a static call Clarion could not bind, kept separate so it is never mixed with resolved evidence). Filter by edge kind (`calls`/`references`) and by a best-effort production/test path heuristic (`all`/`production`/`test`; path partitioning is not indexed — the heuristic matches conventional test paths). Output is bounded; truncated flags when the site cap trims. No LLM call."
        );
        assert_eq!(tools[13].name, "orientation_pack");
        assert_eq!(
            tools[13].description,
            "Assemble one deterministic orientation packet for a code location — the replacement for hand-composing find_entity + entity_at + source reads + neighborhood + issues_for + freshness on every question. Resolve EITHER by `entity` id OR by `file`+`line` (exactly one form). The packet bundles: the primary entity, the entity_context evidence (match_reason / containing stack / decl-body-decorator ranges — so a decorator-line query is explained, not guessed), a compact source-span summary, one-hop neighbors (callers, callees, container, contained, references, imports — for a module, references_in/out are rolled up over contained symbols with references_rolled_up=true), compact resolved execution paths, related Filigree issues, index/Filigree/LLM health, warnings, and suggested next reads. No LLM summary is invoked. Every list is bounded; an `omitted` block reports per-section truncation counts and `degraded` sections name surfaces that were unavailable (e.g. Filigree down) so an empty section is never read as a guaranteed negative."
        );
        assert_eq!(tools[14].name, "analyze_start");
        assert_eq!(
            tools[14].description,
            "Start a `clarion analyze` run over this project in the background and return its run handle immediately — do not block on the (possibly many-minute) run. Re-indexes the source tree and refreshes entities/edges/subsystems. Returns run_id, status (`started`), and the progress-file path. Only one analyze may run per project at a time (a cross-process lock enforces it); a second start while one is active is rejected. Poll analyze_status for progress; analyze_cancel to stop. No arguments."
        );
        assert_eq!(tools[15].name, "analyze_status");
        assert_eq!(
            tools[15].description,
            "Report the live status of an analyze run started via analyze_start. status is one of queued (spawned, not yet recording) | running | completed | failed | cancelled | skipped_no_plugins. While running it exposes phase (discovering / analyzing / clustering), current_plugin, processed_files / total_files, current_file, the latest heartbeat_at, elapsed_seconds, and progress_observed (false when the heartbeat has gone stale — the run may be wedged). On a terminal status it carries the recorded run stats. Reads structured progress, never logs."
        );
        assert_eq!(tools[16].name, "analyze_cancel");
        assert_eq!(
            tools[16].description,
            "Cancel a running analyze. SIGKILLs the run's whole process group — terminating the language plugin and its pyright-langserver child — then marks the run terminal (status `cancelled`) so it is never left dangling as `running`. Idempotent: cancelling an already-terminal run reports its current state. Partial work already written is kept (cancel discards in-flight work, not the index)."
        );
        assert_eq!(tools[17].name, "index_diff");
    }

    #[test]
    fn server_instructions_enumerate_every_tool() {
        // Single-source guard (clarion-71f0d6c3dd): the `instructions` tool list
        // is derived from list_tools(), so every advertised tool must appear in
        // it. If a tool is added/removed and this drifts, the instructions would
        // otherwise silently misdescribe the surface.
        let instructions = super::server_instructions();
        for tool in super::list_tools() {
            assert!(
                instructions.contains(tool.name),
                "instructions omit tool {:?}; instructions were:\n{instructions}",
                tool.name
            );
        }
    }

    #[test]
    fn initialize_returns_server_info_and_tools_capability() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "0.0.0"}
            }
        }))
        .expect("initialize request returns a response");

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 1);
        assert_eq!(
            response["result"]["protocolVersion"],
            super::MCP_PROTOCOL_VERSION
        );
        assert_eq!(response["result"]["serverInfo"]["name"], "clarion");
        assert!(response["result"]["capabilities"]["tools"].is_object());
        // Orientation instructions present and mention the skill + entity model.
        let instructions = response["result"]["instructions"]
            .as_str()
            .expect("initialize result has instructions");
        assert!(
            instructions.contains("clarion-workflow"),
            "instructions should point at the skill"
        );
        assert!(
            instructions.contains("entity"),
            "instructions should describe the entity model"
        );
        // The stateless free handler does NOT serve resources/* or prompts/*,
        // so it must not advertise those capabilities (a client enabling those
        // flows against the stateless server would otherwise fail).
        assert!(
            response["result"]["capabilities"]["prompts"].is_null(),
            "stateless initialize must not advertise prompts: {response:?}"
        );
        assert!(
            response["result"]["capabilities"]["resources"].is_null(),
            "stateless initialize must not advertise resources: {response:?}"
        );
    }

    #[tokio::test]
    async fn stateful_initialize_advertises_prompts_and_resources() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("clarion.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "test-client", "version": "0.0.0"}
                }
            }))
            .await
            .expect("initialize request returns a response");

        // The production (ServerState) path serves prompts/* and resources/*,
        // so its initialize must advertise the full capability surface.
        assert!(response["result"]["capabilities"]["tools"].is_object());
        assert!(
            response["result"]["capabilities"]["prompts"].is_object(),
            "stateful initialize must advertise prompts: {response:?}"
        );
        assert!(
            response["result"]["capabilities"]["resources"].is_object(),
            "stateful initialize must advertise resources: {response:?}"
        );
        let instructions = response["result"]["instructions"]
            .as_str()
            .expect("initialize result has instructions");
        assert!(
            instructions.contains("clarion-workflow"),
            "instructions should point at the skill"
        );
    }

    #[tokio::test]
    async fn resources_list_includes_clarion_context() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("clarion.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "resources/list",
                "params": {}
            }))
            .await
            .expect("response");

        let resources = response["result"]["resources"].as_array().unwrap();
        assert!(
            resources.iter().any(|r| r["uri"] == "clarion://context"),
            "clarion://context not listed: {resources:?}"
        );
    }

    #[tokio::test]
    async fn resources_read_returns_context_snapshot_json() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("clarion.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
            conn.execute(
                "INSERT INTO entities \
                 (id, plugin_id, kind, name, short_name, properties, created_at, updated_at) \
                 VALUES ('python:module:m','python','module','m','m','{}', \
                         '2026-01-01T00:00:00.000Z','2026-01-01T00:00:00.000Z')",
                [],
            )
            .unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "resources/read",
                "params": {"uri": "clarion://context"}
            }))
            .await
            .expect("response");

        let text = response["result"]["contents"][0]["text"]
            .as_str()
            .expect("snapshot text");
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["db_present"], true);
        assert_eq!(parsed["entity_count"], 1);
        assert_eq!(parsed["staleness"], "never_analyzed");
        // A healthy read carries `degraded: false`, distinguishing it from the
        // reader-error fallback which sets `degraded: true`.
        assert_eq!(
            parsed["degraded"], false,
            "healthy snapshot must not be degraded"
        );
    }

    #[tokio::test]
    async fn resources_read_rejects_unknown_uri() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("clarion.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 8,
                "method": "resources/read",
                "params": {"uri": "clarion://nope"}
            }))
            .await
            .expect("response");
        assert!(response["error"].is_object(), "expected an error envelope");
        assert_eq!(response["error"]["code"], -32602, "{response:?}");
    }

    #[tokio::test]
    async fn prompts_get_rejects_unknown_name() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("clarion.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 11,
                "method": "prompts/get",
                "params": {"name": "nope"}
            }))
            .await
            .expect("response");
        assert!(response["error"].is_object(), "expected an error envelope");
        assert_eq!(response["error"]["code"], -32602, "{response:?}");
    }

    #[tokio::test]
    async fn prompts_get_returns_skill_text() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("clarion.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "prompts/get",
                "params": {"name": "clarion-workflow"}
            }))
            .await
            .expect("response");
        let text = response["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(
            text.contains("name: clarion-workflow"),
            "not the skill text"
        );
    }

    #[tokio::test]
    async fn prompts_list_includes_clarion_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("clarion.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "prompts/list",
                "params": {}
            }))
            .await
            .expect("response");
        let prompts = response["result"]["prompts"].as_array().unwrap();
        assert!(prompts.iter().any(|p| p["name"] == "clarion-workflow"));
    }

    #[test]
    fn tools_list_request_wraps_all_tools() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": "tools-1",
            "method": "tools/list",
            "params": {}
        }))
        .expect("tools/list request returns a response");

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], "tools-1");
        assert_eq!(response["result"]["tools"].as_array().unwrap().len(), 18);
        assert_eq!(response["result"]["tools"][0]["name"], "entity_at");
        assert_eq!(response["result"]["tools"][7]["name"], "subsystem_members");
    }

    #[test]
    fn unknown_method_is_json_rpc_method_not_found() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7,
            "method": "not/real",
            "params": {}
        }))
        .expect("unknown request returns a JSON-RPC error response");

        assert_eq!(response["error"]["code"], -32601);
    }

    #[test]
    fn json_rpc_notification_does_not_return_response() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }));

        assert!(response.is_none());
    }

    #[test]
    fn stateless_call_tool_requires_server_state_before_tool_validation() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "tools/call",
            "params": {"name": "not_a_tool", "arguments": {}}
        }))
        .expect("tools/call request returns a response");

        assert_eq!(response["error"]["code"], -32601);
        assert_eq!(
            response["error"]["message"],
            "tools/call requires ServerState::handle_json_rpc"
        );
    }

    #[test]
    fn stateless_json_rpc_does_not_fake_tool_calls() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 88,
            "method": "tools/call",
            "params": {"name": "summary", "arguments": {"id": "python:function:demo.entry"}}
        }))
        .expect("tools/call request returns a response");

        assert_eq!(response["error"]["code"], -32601);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("ServerState")
        );
    }

    #[test]
    fn stateless_call_tool_with_invalid_params_requires_server_state() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tools/call",
            "params": {"arguments": {}}
        }))
        .expect("tools/call request returns a response");

        assert_eq!(response["error"]["code"], -32601);
        assert_eq!(
            response["error"]["message"],
            "tools/call requires ServerState::handle_json_rpc"
        );
    }

    #[test]
    fn frame_dispatch_decodes_and_reencodes_json_rpc() {
        let frame = clarion_core::plugin::Frame {
            body: serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 10,
                "method": "tools/list",
                "params": {}
            }))
            .unwrap(),
        };

        let response = super::handle_frame(&frame)
            .unwrap()
            .expect("request frame returns a response");
        let decoded: serde_json::Value = serde_json::from_slice(&response.body).unwrap();

        assert_eq!(decoded["jsonrpc"], "2.0");
        assert_eq!(decoded["id"], 10);
        assert_eq!(decoded["result"]["tools"].as_array().unwrap().len(), 18);
    }

    #[test]
    fn frame_dispatch_returns_none_for_json_rpc_notifications() {
        let frame = clarion_core::plugin::Frame {
            body: serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }))
            .unwrap(),
        };

        let response = super::handle_frame(&frame).unwrap();

        assert!(response.is_none());
    }

    #[test]
    fn serve_stdio_handles_multiple_content_length_frames() {
        let mut input = Vec::new();
        clarion_core::plugin::write_frame(
            &mut input,
            &clarion_core::plugin::Frame {
                body: serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 11,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "test-client", "version": "0.0.0"}
                    }
                }))
                .unwrap(),
            },
        )
        .unwrap();
        clarion_core::plugin::write_frame(
            &mut input,
            &clarion_core::plugin::Frame {
                body: serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 12,
                    "method": "tools/list",
                    "params": {}
                }))
                .unwrap(),
            },
        )
        .unwrap();

        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
        let mut output = Vec::new();

        super::serve_stdio(&mut reader, &mut output).unwrap();

        let mut response_reader = std::io::BufReader::new(std::io::Cursor::new(output));
        let first = clarion_core::plugin::read_frame(
            &mut response_reader,
            clarion_core::plugin::ContentLengthCeiling::new(usize::MAX),
        )
        .unwrap();
        let second = clarion_core::plugin::read_frame(
            &mut response_reader,
            clarion_core::plugin::ContentLengthCeiling::new(usize::MAX),
        )
        .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
        let second_json: serde_json::Value = serde_json::from_slice(&second.body).unwrap();

        assert_eq!(first_json["id"], 11);
        assert_eq!(first_json["result"]["serverInfo"]["name"], "clarion");
        assert_eq!(second_json["id"], 12);
        assert_eq!(second_json["result"]["tools"].as_array().unwrap().len(), 18);
    }

    #[test]
    fn serve_stdio_ignores_json_rpc_notifications() {
        let input = notification_sequence_input(13, 14);
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
        let mut output = Vec::new();

        super::serve_stdio(&mut reader, &mut output).unwrap();
        assert_notification_sequence_responses(output, 13, 14);
    }

    #[test]
    fn serve_stdio_with_state_ignores_json_rpc_notifications() {
        let project = tempfile::tempdir().expect("temp project");
        let db_path = project.path().join("clarion.db");
        let mut conn = Connection::open(&db_path).expect("open sqlite");
        pragma::apply_write_pragmas(&conn).expect("write pragmas");
        schema::apply_migrations(&mut conn).expect("apply migrations");
        drop(conn);

        let readers = ReaderPool::open(&db_path, 1).expect("reader pool");
        let state = ServerState::new(project.path().to_path_buf(), readers);
        let input = notification_sequence_input(15, 16);
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
        let mut output = Vec::new();

        super::serve_stdio_with_state(&state, &mut reader, &mut output).unwrap();
        assert_notification_sequence_responses(output, 15, 16);
    }

    #[test]
    fn serve_stdio_with_state_uses_json_line_transport_for_json_line_requests() {
        let project = tempfile::tempdir().expect("temp project");
        let db_path = project.path().join("clarion.db");
        let mut conn = Connection::open(&db_path).expect("open sqlite");
        pragma::apply_write_pragmas(&conn).expect("write pragmas");
        schema::apply_migrations(&mut conn).expect("apply migrations");
        drop(conn);

        let readers = ReaderPool::open(&db_path, 1).expect("reader pool");
        let state = ServerState::new(project.path().to_path_buf(), readers);
        let input = notification_sequence_json_lines(17, 18);
        let mut reader = std::io::BufReader::new(std::io::Cursor::new(input));
        let mut output = Vec::new();

        super::serve_stdio_with_state(&state, &mut reader, &mut output).unwrap();
        assert_notification_sequence_json_lines(output, 17, 18);
    }

    fn notification_sequence_input(initialize_id: u64, tools_list_id: u64) -> Vec<u8> {
        let mut input = Vec::new();
        clarion_core::plugin::write_frame(
            &mut input,
            &clarion_core::plugin::Frame {
                body: serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": initialize_id,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2025-11-25",
                        "capabilities": {},
                        "clientInfo": {"name": "test-client", "version": "0.0.0"}
                    }
                }))
                .unwrap(),
            },
        )
        .unwrap();
        clarion_core::plugin::write_frame(
            &mut input,
            &clarion_core::plugin::Frame {
                body: serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized",
                    "params": {}
                }))
                .unwrap(),
            },
        )
        .unwrap();
        clarion_core::plugin::write_frame(
            &mut input,
            &clarion_core::plugin::Frame {
                body: serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": tools_list_id,
                    "method": "tools/list",
                    "params": {}
                }))
                .unwrap(),
            },
        )
        .unwrap();
        input
    }

    fn assert_notification_sequence_responses(
        output: Vec<u8>,
        initialize_id: u64,
        tools_list_id: u64,
    ) {
        let mut response_reader = std::io::BufReader::new(std::io::Cursor::new(output));
        let first = clarion_core::plugin::read_frame(
            &mut response_reader,
            clarion_core::plugin::ContentLengthCeiling::new(usize::MAX),
        )
        .unwrap();
        let second = clarion_core::plugin::read_frame(
            &mut response_reader,
            clarion_core::plugin::ContentLengthCeiling::new(usize::MAX),
        )
        .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
        let second_json: serde_json::Value = serde_json::from_slice(&second.body).unwrap();

        assert_eq!(first_json["id"], initialize_id);
        assert_eq!(second_json["id"], tools_list_id);
        assert!(
            clarion_core::plugin::read_frame(
                &mut response_reader,
                clarion_core::plugin::ContentLengthCeiling::new(usize::MAX),
            )
            .is_err(),
            "notifications must not produce JSON-RPC response frames"
        );
    }

    fn notification_sequence_json_lines(initialize_id: u64, tools_list_id: u64) -> Vec<u8> {
        let messages = [
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": initialize_id,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "test-client", "version": "0.0.0"}
                }
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": tools_list_id,
                "method": "tools/list",
                "params": {}
            }),
        ];
        let mut input = Vec::new();
        for message in messages {
            serde_json::to_writer(&mut input, &message).expect("serialize json line");
            input.push(b'\n');
        }
        input
    }

    fn assert_notification_sequence_json_lines(
        output: Vec<u8>,
        initialize_id: u64,
        tools_list_id: u64,
    ) {
        let output = String::from_utf8(output).expect("json lines are utf8");
        let lines: Vec<_> = output.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "notifications must not produce JSON-RPC response lines"
        );
        let first_json: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let second_json: serde_json::Value = serde_json::from_str(lines[1]).unwrap();

        assert_eq!(first_json["id"], initialize_id);
        assert_eq!(second_json["id"], tools_list_id);
    }

    #[tokio::test]
    async fn inferred_inflight_entry_is_removed_when_leader_future_is_aborted() {
        let project = tempfile::tempdir().expect("temp project");
        let db_path = project.path().join("clarion.db");
        let mut conn = Connection::open(&db_path).expect("open sqlite");
        pragma::apply_write_pragmas(&conn).expect("write pragmas");
        schema::apply_migrations(&mut conn).expect("apply migrations");
        drop(conn);

        let readers = ReaderPool::open(&db_path, 1).expect("reader pool");
        let state = Arc::new(ServerState::new(project.path().to_path_buf(), readers));
        let key = inferred_test_key();
        let read = inferred_test_read(key.clone());
        let (writer, _rx) = mpsc::channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let llm = InferenceLlmState {
            writer,
            config: LlmConfig::default(),
            provider: Arc::new(BlockingProvider {
                release: Mutex::new(release_rx),
            }),
        };

        let leader_state = Arc::clone(&state);
        let leader_key = key.clone();
        let handle = tokio::spawn(async move {
            leader_state
                .coalesced_inferred_dispatch(leader_key, read, llm)
                .await
        });
        assert_inferred_inflight_contains(&state, &key).await;

        handle.abort();
        let _ = handle.await;
        let removed = wait_until_inferred_inflight_removed(&state, &key).await;
        let _ = release_tx.send(());

        assert!(
            removed,
            "aborted inferred-dispatch leader left stale in-flight key"
        );
    }

    async fn assert_inferred_inflight_contains(state: &ServerState, key: &InferredEdgeCacheKey) {
        for _ in 0..50 {
            if state.inferred_inflight.lock().await.contains_key(key) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("inferred-dispatch leader never registered in-flight key");
    }

    async fn wait_until_inferred_inflight_removed(
        state: &ServerState,
        key: &InferredEdgeCacheKey,
    ) -> bool {
        for _ in 0..50 {
            if !state.inferred_inflight.lock().await.contains_key(key) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    fn inferred_test_key() -> InferredEdgeCacheKey {
        InferredEdgeCacheKey {
            caller_entity_id: "python:function:demo.dynamic".to_owned(),
            caller_content_hash: "hash-caller".to_owned(),
            model_id: "test-model".to_owned(),
            prompt_version: "test-prompt".to_owned(),
        }
    }

    fn inferred_test_read(key: InferredEdgeCacheKey) -> InferredRead {
        InferredRead {
            caller: entity_row(&key.caller_entity_id, "dynamic", Some("hash-caller")),
            sites: vec![UnresolvedCallSiteRow {
                caller_entity_id: key.caller_entity_id.clone(),
                caller_content_hash: key.caller_content_hash.clone(),
                site_key: "site-1".to_owned(),
                site_ordinal: 0,
                source_file_id: Some("python:module:demo".to_owned()),
                source_byte_start: 0,
                source_byte_end: 8,
                callee_expr: "target()".to_owned(),
            }],
            candidates: vec![entity_row(
                "python:function:demo.target",
                "target",
                Some("hash-target"),
            )],
            key,
            cached: None,
        }
    }

    fn entity_row(id: &str, name: &str, content_hash: Option<&str>) -> EntityRow {
        EntityRow {
            id: id.to_owned(),
            plugin_id: "python".to_owned(),
            kind: "function".to_owned(),
            name: name.to_owned(),
            short_name: name.to_owned(),
            parent_id: Some("python:module:demo".to_owned()),
            source_file_id: Some("python:module:demo".to_owned()),
            source_file_path: None,
            source_byte_start: Some(0),
            source_byte_end: Some(8),
            source_line_start: Some(1),
            source_line_end: Some(1),
            properties_json: "{}".to_owned(),
            content_hash: content_hash.map(str::to_owned),
            summary_json: None,
        }
    }

    struct BlockingProvider {
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl LlmProvider for BlockingProvider {
        fn name(&self) -> &'static str {
            "blocking"
        }

        fn invoke(&self, _request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
            let _ = self
                .release
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv();
            Ok(LlmResponse {
                model_id: "test-model".to_owned(),
                output_json: r#"{"edges":[]}"#.to_owned(),
                input_tokens: 1,
                cached_input_tokens: 0,
                output_tokens: 1,
                total_tokens: 2,
                cost_usd: 0.0,
            })
        }

        fn estimate_tokens(&self, _request: &LlmRequest) -> u64 {
            1
        }

        fn tier_to_model(&self, _tier: &str) -> Option<&str> {
            Some("test-model")
        }

        fn caching_model(&self) -> CachingModel {
            CachingModel::OpenAiChatCompletions
        }
    }
}
