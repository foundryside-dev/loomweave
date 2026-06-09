//! MCP protocol surface for Loomweave.

mod analyze_runs;
mod catalogue;
pub mod config;
pub mod filigree;
pub mod filigree_url;
mod index_diff;
pub mod scan_results;
pub mod snapshot;
mod tools;
pub mod wardline_reconcile;

use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use loomweave_core::{
    EdgeConfidence, EmbeddingProvider, LlmProvider, LlmProviderError, LlmRequest, LlmResponse,
    McpErrorCode,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use time::{Date, Month, OffsetDateTime, macros::format_description};
use tokio::sync::{Mutex as AsyncMutex, Notify, broadcast, mpsc};

use loomweave_core::plugin::{ContentLengthCeiling, Frame, TransportError};
use loomweave_storage::{
    CallEdgeMatch, EntityRow, InferredCallEdgeRecord, InferredEdgeCacheEntry, InferredEdgeCacheKey,
    InferredEdgeWriteStats, ReaderPool, ReferenceDirection, ReferenceEdgeMatch,
    RolledUpReferenceEdge, StorageError, SummaryCacheEntry, SummaryCacheKey, UnresolvedCallSiteRow,
    WriterCmd, call_edges_from, call_edges_targeting, containing_module_id,
    entity_briefing_block_reason, entity_by_id, import_edges_for_entity,
    inferred_edge_cache_key_id, module_reference_rollup, reference_edges_for_entity,
    sei_for_locator, unresolved_call_sites_for_caller, unresolved_callers_for_target,
};

use crate::config::{LlmConfig, SemanticSearchConfig};
use crate::filigree::{
    EntityAssociation, EntityAssociationsResponse, FiligreeLookup, IssueDetail,
    ObservationCreateRequest,
};
use loomweave_storage::{
    GuidanceProposal, GuidanceSheetInput, invalidate_summaries_for_sheet, upsert_guidance_sheet,
};

/// MCP protocol revision supported by the B.6 stdio server.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";
const EMPTY_GUIDANCE_FINGERPRINT: &str = "guidance-empty";

/// The bundled loomweave-workflow skill text, embedded for the `prompts/get`
/// surface and reused as the canonical orientation reference. The asset lives
/// in this crate's own tree; the CLI (which depends on loomweave-mcp) reaches
/// down into it to embed the same bytes for its on-disk `install --skills`
/// copy (clarion-04391392c7).
pub const LOOMWEAVE_WORKFLOW_SKILL: &str =
    include_str!("../assets/skills/loomweave-workflow/SKILL.md");

/// Orientation text returned in the MCP `initialize` result's `instructions`
/// field. The `Tools:` enumeration is derived from [`list_tools_for_policy`]
/// under the *active* policy (the single source of truth) so it can never
/// advertise a tool the server will not actually register — the
/// agent-first-feedback §2.5 bug, where the write tools were listed but absent
/// from `tools/list` unless `serve.mcp.enable_write_tools: true`. When write
/// tools are gated off, a note names them and how to enable them. Kept
/// consistent with the loomweave-workflow skill.
fn server_instructions(policy: McpToolPolicy) -> String {
    let tool_names = list_tools_for_policy(policy)
        .iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>()
        .join(", ");
    let write_tools_note = if policy.enable_write_tools {
        String::new()
    } else {
        "\n\nNot listed above (write-gated): `entity_summary_get`, `analyze_start`, \
`analyze_cancel`, `propose_guidance`, `promote_guidance`. These require \
`serve.mcp.enable_write_tools: true` in loomweave.yaml; until then they are not \
registered and calling one returns a tool-disabled error."
            .to_owned()
    };
    format!(
        "Loomweave is a code-archaeology server: it has pre-extracted this project \
into a queryable map of entities (functions, classes, modules, files), the call \
/ reference / import edges between them, and subsystem clusters. Ask Loomweave \
instead of re-reading or grepping the tree.

Entity IDs are `{{plugin}}:{{kind}}:{{qualified_name}}` (e.g. \
`python:function:pkg.mod.func`); subsystems are `core:subsystem:{{hash}}`. You \
almost never type IDs — get one from `entity_find` or `entity_at`, then copy it \
verbatim into the next tool.

Tools: {tool_names}. `entity_callers_list` / `entity_neighborhood_get` / `entity_execution_path_list` \
take a `confidence` tier (resolved | ambiguous | inferred; default resolved). \
`project_status_get` reports index freshness, counts, LLM policy, and the resolved \
Filigree endpoint.{write_tools_note}

For the full workflow see the loomweave-workflow skill (installed by \
`loomweave install --skills`), or read the `loomweave-workflow` prompt. Live \
project counts and index freshness are in the `loomweave://context` resource."
    )
}

type InferredInflight =
    Arc<AsyncMutex<HashMap<InferredEdgeCacheKey, broadcast::Sender<InferredDispatchOutcome>>>>;

pub const RENAME_MAP: &[(&str, &str)] = &[
    ("entity_at", "entity_at"),
    ("find_entity", "entity_find"),
    ("callers_of", "entity_callers_list"),
    ("execution_paths_from", "entity_execution_path_list"),
    ("summary", "entity_summary_get"),
    ("issues_for", "entity_issue_list"),
    ("neighborhood", "entity_neighborhood_get"),
    ("subsystem_members", "subsystem_member_list"),
    ("subsystem_of", "entity_subsystem_get"),
    ("project_status", "project_status_get"),
    ("summary_preview_cost", "entity_summary_preview_cost_get"),
    ("source_for_entity", "entity_source_get"),
    ("call_sites", "entity_call_site_list"),
    ("orientation_pack", "entity_orientation_pack_get"),
    ("analyze_start", "analyze_start"),
    ("analyze_status", "analyze_status_get"),
    ("analyze_cancel", "analyze_cancel"),
    ("index_diff", "index_diff_get"),
    ("guidance_for", "entity_guidance_list"),
    ("propose_guidance", "propose_guidance"),
    ("promote_guidance", "promote_guidance"),
    ("findings_for", "entity_finding_list"),
    ("wardline_for", "entity_wardline_get"),
    ("find_by_tag", "entity_tag_list"),
    ("find_by_kind", "entity_kind_list"),
    ("find_by_wardline", "entity_wardline_list"),
    ("find_circular_imports", "module_circular_import_list"),
    ("find_coupling_hotspots", "entity_coupling_hotspot_list"),
    ("find_entry_points", "entity_entry_point_list"),
    ("find_http_routes", "entity_http_route_list"),
    ("find_data_models", "entity_data_model_list"),
    ("find_tests", "entity_test_list"),
    ("find_deprecations", "entity_deprecation_list"),
    ("find_todos", "entity_todo_list"),
    ("what_tests_this", "entity_test_caller_list"),
    ("high_churn", "entity_high_churn_list"),
    ("recently_changed", "entity_recent_change_list"),
    ("find_dead_code", "entity_dead_list"),
    ("search_semantic", "entity_semantic_search_list"),
];

pub fn rename_old_to_new(name: &str) -> &str {
    for &(old, new) in RENAME_MAP {
        if name == old {
            return new;
        }
    }
    name
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ToolMetadata {
    pub read_only: bool,
    pub writes_local_state: bool,
    pub writes_external_state: bool,
    pub spawns_process: bool,
    pub may_call_llm: bool,
}

impl ToolMetadata {
    const fn read_only() -> Self {
        Self {
            read_only: true,
            writes_local_state: false,
            writes_external_state: false,
            spawns_process: false,
            may_call_llm: false,
        }
    }

    #[allow(clippy::fn_params_excessive_bools)]
    const fn write_tool(
        writes_local_state: bool,
        writes_external_state: bool,
        spawns_process: bool,
        may_call_llm: bool,
    ) -> Self {
        Self {
            read_only: false,
            writes_local_state,
            writes_external_state,
            spawns_process,
            may_call_llm,
        }
    }

    const fn conditional_llm() -> Self {
        Self {
            read_only: true,
            writes_local_state: false,
            writes_external_state: false,
            spawns_process: false,
            may_call_llm: true,
        }
    }
}

pub fn tool_metadata(name: &str) -> ToolMetadata {
    match name {
        "entity_summary_get" => ToolMetadata::write_tool(true, false, false, true),
        "entity_callers_list" | "entity_neighborhood_get" | "entity_execution_path_list" => {
            ToolMetadata::conditional_llm()
        }
        "analyze_start" => ToolMetadata::write_tool(true, false, true, false),
        "analyze_cancel" | "promote_guidance" => {
            ToolMetadata::write_tool(true, false, false, false)
        }
        "propose_guidance" => ToolMetadata::write_tool(false, true, false, false),
        _ => ToolMetadata::read_only(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct McpToolPolicy {
    pub enable_write_tools: bool,
}

impl McpToolPolicy {
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            enable_write_tools: false,
        }
    }

    #[must_use]
    pub const fn allow_write_tools() -> Self {
        Self {
            enable_write_tools: true,
        }
    }

    #[must_use]
    pub fn allows(self, name: &str) -> bool {
        self.enable_write_tools || tool_metadata(name).read_only
    }

    fn allows_arguments(self, name: &str, arguments: &serde_json::Map<String, Value>) -> bool {
        self.enable_write_tools || !tool_uses_conditional_inferred_dispatch(name, arguments)
    }
}

fn tool_uses_conditional_inferred_dispatch(
    name: &str,
    arguments: &serde_json::Map<String, Value>,
) -> bool {
    if !matches!(
        name,
        "entity_callers_list" | "entity_neighborhood_get" | "entity_execution_path_list"
    ) {
        return false;
    }
    matches!(
        arguments.get("confidence").and_then(Value::as_str),
        Some("inferred" | "all")
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

impl Serialize for ToolDefinition {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let metadata = tool_metadata(self.name);
        let mut state = serializer.serialize_struct("ToolDefinition", 9)?;
        state.serialize_field("name", self.name)?;
        state.serialize_field("description", self.description)?;
        state.serialize_field("inputSchema", &self.input_schema)?;
        state.serialize_field("metadata", &metadata)?;
        state.serialize_field("read_only", &metadata.read_only)?;
        state.serialize_field("writes_local_state", &metadata.writes_local_state)?;
        state.serialize_field("writes_external_state", &metadata.writes_external_state)?;
        state.serialize_field("spawns_process", &metadata.spawns_process)?;
        state.serialize_field("may_call_llm", &metadata.may_call_llm)?;
        state.end()
    }
}

#[must_use]
// A flat registry of tool definitions; length tracks the tool count by design.
#[allow(clippy::too_many_lines)]
pub fn list_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "entity_at",
            description: "Return the innermost Loomweave entity whose source range contains a file and line, plus an `entity_context` evidence block: match_reason (decorator_range / declaration / body_range / containing_range / no_match) explaining why the line matched, the module→entity containing stack, the matched entity's decl/body/decorator sub-ranges, any same-granularity ambiguity alternatives, and index freshness. Paths are normalized relative to the project root. A blank or comment line that only a module spans reports containing_range — never a fabricated exact match.",
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
            name: "entity_find",
            description: "Search Loomweave entities by id, name, short name, summary, and docstring content. Matching merges stemmed FTS ranking with grep-equivalent substring recall, so a concept word finds both entities whose docstring mentions it and identifiers that merely contain it (e.g. `library` finds the class `LibraryService`, which whole-token FTS alone misses). This is the always-on keyword-discovery path — no embeddings required (semantic ranking is the separate, opt-in `entity_semantic_search_list`). Results are paginated; FTS-ranked hits come first, then substring-only hits. Docstrings withheld by the secret scanner (briefing_blocked) are never matched. This does not traverse the graph and does not search on-demand summary_cache entries. Pass an optional `kind` (e.g. \"subsystem\", \"function\", \"class\", \"module\") to return only entities of that kind — the way to locate a subsystem without visually filtering results.",
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
            name: "entity_callers_list",
            description: "Return entities that call the given entity. Default confidence is resolved, so ambiguous static candidates and LLM-inferred edges are excluded unless explicitly requested. Ambiguous edges expand all candidates; inferred edges may trigger bounded LLM dispatch. The result carries scope_excludes naming static blind spots not searched (e.g. attribute-receiver-calls) so an empty callers list is never read as a guaranteed true negative.",
            input_schema: id_confidence_schema(),
        },
        ToolDefinition {
            name: "entity_execution_path_list",
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
            name: "entity_summary_get",
            description: "Return an on-demand cached summary for one entity. In v0.1 this is leaf scope only: module summaries describe the module docstring and top-level members, not an aggregation of contained function/class summaries. If the LLM returns non-JSON the response degrades to a deterministic structural summary (kind: structural-fallback) built from the entity source, and that fallback is cached so a retry is a free cache hit rather than a re-billed failure.",
            input_schema: id_schema(),
        },
        ToolDefinition {
            name: "entity_issue_list",
            description: "Return Filigree issues attached to this Loomweave entity, optionally including issues attached to contained entities. Filigree is an enrichment source; if unavailable, the tool returns an unavailable envelope instead of failing Loomweave. The result carries a result_kind (matched | no_matches | unavailable) so a reachable-but-empty Filigree is distinct from an unreachable one, and a filigree_endpoint block (configured vs resolved URL + resolution_source) so you can see which endpoint — e.g. a live ethereal port — the answer came from. Each matched/drifted entry carries an `issue` object with the issue's title, status, and priority (fetched once per distinct issue, no N+1); `issue` is null when the issue-detail route is unavailable, so the match still resolves without a second hop into Filigree. Includes a `wardline_findings` section (enrich-only) reconciling Wardline findings to the entity by qualname; `result_kind` is matched|no_matches|unavailable.",
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
            name: "entity_neighborhood_get",
            description: "Return the one-hop Loomweave neighborhood around an entity: callers, callees, container, contained entities, references, and imports (imports_in = who imports this module, imports_out = what it imports; module-to-module). Default confidence is resolved; ambiguous and inferred calls are opt-in. References and imports are not execution flow. When the entity is a module, references_in/references_out are rolled up over the symbols it contains (references_rolled_up=true) — each neighbor carries a `via` naming the contained symbol the edge touches, so \"who imports this module/contract\" is answered at module altitude rather than reading empty. On references_in each rolled-up neighbor also carries `importer_module` — the importing symbol's containing module — so reverse-import names importing modules, not just symbols. The result carries scope_excludes naming blind spots not searched (e.g. attribute-receiver-calls) so empty sections are never read as guaranteed true negatives.",
            input_schema: id_confidence_schema(),
        },
        ToolDefinition {
            name: "subsystem_member_list",
            description: "List module entities assigned to a subsystem entity.",
            input_schema: id_schema(),
        },
        ToolDefinition {
            name: "entity_subsystem_get",
            description: "Return the subsystem an entity belongs to — the reverse of subsystem_members. Accepts any entity id: a module resolves directly, while a function/class resolves through its nearest containing module. Returns the subsystem id/name and the module the membership was resolved through, or a no-subsystem result when the entity has no subsystem-assigned module ancestor.",
            input_schema: id_schema(),
        },
        ToolDefinition {
            name: "project_status_get",
            description: "Return deterministic Loomweave diagnostics: repo root, db path, latest run (id/status/started/completed), entity/subsystem/edge/finding/briefing-blocked counts, index staleness, per-plugin entity counts from the current index, LLM policy (provider/live/cache), and the resolved Filigree endpoint (configured vs resolved URL + resolution source). Answers \"is the graph fresh, plugin-less, LLM-live, Filigree-reachable?\" without shelling out. No LLM call.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "entity_summary_preview_cost_get",
            description: "Preview what calling summary(id) would cost BEFORE spending. Reports cache_status (hit | expired | miss), the cached row's real tokens/cost/age on a hit, an input-token estimate on a miss, the configured model, the LLM policy (provider/live/allow_live_provider/cache horizon), and live_spend_would_occur — true only when no fresh cache row exists AND a live provider is wired. A disabled/unconfigured LLM is reported distinctly from a cache miss. Never invokes the LLM provider.",
            input_schema: id_schema(),
        },
        ToolDefinition {
            name: "entity_source_get",
            description: "Return the exact indexed source span for one entity (its source_line_start..source_line_end, which includes any decorators/signature/docstring the plugin captured) plus a bounded window of surrounding context, as line-numbered lines each flagged in_entity true/false. No LLM call. Lets an agent read and trust the entity without shelling out. source_status reports `ok`, or — instead of a misleading stale snippet — `missing` (file gone), `no_range`/`no_source_path` (entity has no anchor), `binary` (non-UTF-8), or `drifted` (the file no longer matches the indexed content_hash; rerun `loomweave analyze`). context_lines defaults to 10.",
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
            name: "entity_call_site_list",
            description: "Show the actual source sites behind calls/references edges, so an agent can see WHY Loomweave believes an edge exists rather than trusting it blind. role=caller (default) returns this entity's outgoing sites (what it calls/references); role=callee returns incoming sites (who calls/references it). Each site carries the file path, 1-based line, byte column, the source line text, edge kind, confidence, and a resolution of resolved | ambiguous (with candidate ids) | unresolved (a static call Loomweave could not bind, kept separate so it is never mixed with resolved evidence). Filter by edge kind (`calls`/`references`) and by a best-effort production/test path heuristic (`all`/`production`/`test`; path partitioning is not indexed — the heuristic matches conventional test paths). Output is bounded; truncated flags when the site cap trims. No LLM call.",
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
            name: "entity_orientation_pack_get",
            description: "Assemble one deterministic orientation packet for a code location — the replacement for hand-composing find_entity + entity_at + source reads + neighborhood + issues_for + freshness on every question. Resolve EITHER by `entity` id OR by `file`+`line` (exactly one form). The packet bundles: the primary entity, the entity_context evidence (match_reason / containing stack / decl-body-decorator ranges — so a decorator-line query is explained, not guessed), a compact source-span summary, one-hop neighbors (callers, callees, container, contained, references, imports — for a module, references_in/out are rolled up over contained symbols with references_rolled_up=true), compact resolved execution paths, related Filigree issues, index/Filigree/LLM health, warnings, and suggested next reads. No LLM summary is invoked. Every list is bounded; an `omitted` block reports per-section truncation counts and `degraded` sections name surfaces that were unavailable (e.g. Filigree down) so an empty section is never read as a guaranteed negative. Includes a `wardline_findings` section (enrich-only) reconciling Wardline findings to the entity by qualname; `result_kind` is matched|no_matches|unavailable.",
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
            description: "Start a `loomweave analyze` run over this project in the background and return its run handle immediately — do not block on the (possibly many-minute) run. Re-indexes the source tree and refreshes entities/edges/subsystems. Returns run_id, status (`started`), and the progress-file path. Only one analyze may run per project at a time (a cross-process lock enforces it); a second start while one is active is rejected. Poll analyze_status for progress; analyze_cancel to stop. No arguments.",
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "analyze_status_get",
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
            name: "index_diff_get",
            description: "Report what changed since the last analyze and whether this checkout is newer than the graph — so an agent need not hand-roll git + mtime freshness checks. Compares: analyzed_commit (the persisted commit analyzed by the latest completed run) vs current git HEAD when both are known, falling back to analyzed_at vs HEAD committer date when needed; indexed source files modified or now-missing since analyze; dirty working-tree files flagged when they touch an indexed path; and per-run aggregate plugin skip/drop counters. Git is read at query time, read-only, and fail-soft: a missing git binary or non-repo dir degrades to git.available=false with a reason rather than failing. overall is fresh | drift | unknown | never_analyzed; lists are bounded with an `omitted` block. entity-level add/remove/change diff is unavailable in v0.1 (only the current graph is retained). No LLM call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "minimum": 1, "maximum": 2000}
                },
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "entity_guidance_list",
            description: "Return the guidance sheets applicable to one entity, composed at query time and ranked by scope_rank (project → subsystem → package → module → class → function), ties broken by authored_at then id. Read-only: this surfaces composed institutional knowledge; authoring (propose/promote) is a separate lifecycle. A sheet applies via an explicit `guides` edge OR a `match_rules` entry resolved against the entity (path glob / tag / kind / subsystem / entity). `wardline_group` rules are not evaluated here (the Wardline blob is opaque) and are reported in `notes`, never guessed. Expired sheets are excluded. Each sheet carries its `sei`. Bounded (limit/offset, page.total/truncated). Honest-empty when no sheet applies. No LLM call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "propose_guidance",
            description: "Propose a guidance sheet for operator review by creating a Filigree observation. This is deliberately inert: it does not write a Loomweave guidance entity and cannot enter summaries until `promote_guidance` or `loomweave guidance promote` consumes the observation.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "entity_id": {"type": "string", "minLength": 1},
                    "content": {"type": "string", "minLength": 1},
                    "scope_level": {
                        "type": "string",
                        "enum": ["project", "subsystem", "package", "module", "class", "function"],
                        "default": "function"
                    },
                    "match_rules": {"type": "array", "items": {"type": "object"}},
                    "name": {"type": "string", "minLength": 1},
                    "pinned": {"type": "boolean", "default": false},
                    "expires": {"type": "string", "minLength": 1}
                },
                "required": ["entity_id", "content"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "promote_guidance",
            description: "Promote a reviewed Filigree observation produced by `propose_guidance` into a local Loomweave guidance sheet. This operator action is the anti-poisoning boundary: only promoted observations become prompt-composed guidance.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "observation_id": {"type": "string", "minLength": 1}
                },
                "required": ["observation_id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "entity_finding_list",
            description: "Return findings anchored to one entity, optionally filtered by `filter.kind` (defect/fact/classification/metric/suggestion), `filter.severity` (INFO/WARN/ERROR/CRITICAL/NONE), and `filter.status` (open/acknowledged/suppressed/promoted_to_issue). The queried entity carries its `sei`; each finding's `related_entities` are raw locator ids (references, not the primary return). Bounded (limit/offset, page.total/truncated). An entity with no findings returns an empty list, not an error. No LLM call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "filter": {
                        "type": "object",
                        "properties": {
                            "kind": {"type": "string"},
                            "severity": {"type": "string"},
                            "status": {"type": "string"}
                        },
                        "additionalProperties": false
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "entity_wardline_get",
            description: "Return the Wardline metadata recorded for one entity (declared tier, groups, boundary contracts) returned VERBATIM — the `wardline_json` blob is opaque to Loomweave. result_kind is `present` when a taint fact exists, else `no_facts` with a missing-signal note: facts are populated via Filigree Flow-B (POST /api/wardline/taint-facts), so a locally-empty result is honest, not an error. The entity carries its `sei`. No LLM call.",
            input_schema: id_schema(),
        },
        ToolDefinition {
            name: "entity_tag_list",
            description: "Return entities carrying a plugin-emitted categorisation `tag`, within an optional `scope` (an entity id → its descendants, OR a path glob like \"src/auth/**\"; omitted → whole project). Bounded (limit/offset, page.total/truncated; scope_truncated/scan_truncated flag cap hits). Entities carry their `sei`. Honest-empty with a missing-signal note when no entity in the current index carries the tag. No LLM call.",
            input_schema: scope_facet_schema(&[("tag", true)]),
        },
        ToolDefinition {
            name: "entity_kind_list",
            description: "Return entities of a plugin-declared `kind` (e.g. \"function\", \"class\", \"module\"), within an optional `scope` (entity id → descendants, OR path glob; omitted → whole project). Bounded (limit/offset, page.total/truncated). Entities carry their `sei`. An unknown kind matches no rows. No LLM call.",
            input_schema: scope_facet_schema(&[("kind", true)]),
        },
        ToolDefinition {
            name: "entity_wardline_list",
            description: "Return entities carrying a Wardline taint fact, optionally filtered by `tier` and/or `group`, within an optional `scope` (entity id → descendants, OR path glob; omitted → whole project). Pass `has_findings: true` to return only entities that ALSO carry at least one finding — page just the fact-carrying-and-flawed entities instead of every taint-fact blob. The Wardline blob is opaque to Loomweave: tier/group filtering is best-effort against a top-level field on the blob and honest-empty when absent. Each entity carries its `wardline` blob verbatim plus its `sei`. Bounded (limit/offset, page.total/truncated). Facts are populated via Filigree Flow-B. No LLM call.",
            input_schema: wardline_facet_schema(),
        },
        ToolDefinition {
            name: "module_circular_import_list",
            description: "Return import cycles in the module import graph (`imports` edges) — each a strongly-connected component of size > 1 (or a self-import), members sorted. On-demand graph query (no analyze-time precompute). Edge-derived: default `confidence` is resolved (the tier is a ceiling — resolved → resolved only, inferred → all) and is echoed in the result. Optional `scope` (entity id → descendants, OR path glob) restricts to cycles whose members are all in scope. Bounded (limit/offset, page.total/truncated). Each member carries its `sei`. No LLM call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "minLength": 1},
                    "confidence": confidence_schema(),
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "entity_coupling_hotspot_list",
            description: "Return entities ranked by coupling (distinct fan-in + fan-out over the edge graph), most-coupled first. On-demand graph query (no analyze-time precompute). Edge-derived: default `confidence` is resolved (a ceiling) and is echoed. Optional `scope` (entity id → descendants, OR path glob; omitted → whole project). Bounded (limit default 20, max 200; page.total/truncated). Each entity carries its `sei`. No LLM call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "minLength": 1},
                    "confidence": confidence_schema(),
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "entity_entry_point_list",
            description: "Return entities tagged as entry points, within an optional `scope` (entity id → descendants, OR path glob). Reads the `entry-point` categorisation tag. HONEST-EMPTY when no entity in the current index carries the tag, so an empty result means the signal is absent, NOT that there are no entry points. Bounded; SEI-carrying. No LLM call.",
            input_schema: scope_page_schema(false),
        },
        ToolDefinition {
            name: "entity_http_route_list",
            description: "Return entities tagged as HTTP routes, within an optional `scope`. Reads the `http-route` categorisation tag. HONEST-EMPTY when route categorisation is not emitted (missing-signal note). Bounded; SEI-carrying. No LLM call.",
            input_schema: scope_page_schema(false),
        },
        ToolDefinition {
            name: "entity_data_model_list",
            description: "Return entities tagged as data models, within an optional `scope`. Reads the `data-model` categorisation tag. HONEST-EMPTY when data-model categorisation is not emitted (missing-signal note). Bounded; SEI-carrying. No LLM call.",
            input_schema: scope_page_schema(false),
        },
        ToolDefinition {
            name: "entity_test_list",
            description: "Return entities tagged as tests, within an optional `scope`. Reads the `test` categorisation tag. HONEST-EMPTY when test categorisation is not emitted (missing-signal note). Bounded; SEI-carrying. No LLM call.",
            input_schema: scope_page_schema(false),
        },
        ToolDefinition {
            name: "entity_deprecation_list",
            description: "Return entities tagged deprecated, within an optional `scope`. Reads the `deprecated` categorisation tag. HONEST-EMPTY when deprecation categorisation is not emitted (missing-signal note). Bounded; SEI-carrying. No LLM call.",
            input_schema: scope_page_schema(false),
        },
        ToolDefinition {
            name: "entity_todo_list",
            description: "Return entities carrying a TODO/FIXME marker, within an optional `scope`. Reads the `todo` categorisation tag. HONEST-EMPTY when TODO extraction is not emitted (missing-signal note). Bounded; SEI-carrying. No LLM call.",
            input_schema: scope_page_schema(false),
        },
        ToolDefinition {
            name: "entity_test_caller_list",
            description: "Return the test entities that exercise an entity — its callers carrying the `test` categorisation tag. HONEST-EMPTY when test categorisation is not emitted, so an empty result is NOT a guarantee the entity is untested (a missing-signal note says so). Bounded; tests carry their `sei`. No LLM call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "required": ["id"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "entity_high_churn_list",
            description: "Return entities ranked by git churn (`git_churn_count`) descending, within an optional `scope`. The analyze pipeline does not populate churn in v1.0, so this is HONEST-EMPTY in practice (missing-signal note); the query is real and lights up if churn is ever populated. Bounded; SEI-carrying. No LLM call.",
            input_schema: scope_page_schema(false),
        },
        ToolDefinition {
            name: "entity_recent_change_list",
            description: "Return entities changed since a timestamp (`since?`), within an optional `scope`. Loomweave does not index a per-entity git change timestamp in v1.0, so this is an HONEST NO-OP: it returns an empty set with a missing-signal note pointing at `index_diff` for repo-level freshness (HEAD vs last analyze). Never fabricates a change set. No LLM call.",
            input_schema: scope_page_schema(true),
        },
        ToolDefinition {
            name: "entity_dead_list",
            description: "Return entities NOT reachable from the root set (entry points ∪ exported API ∪ tests ∪ HTTP routes ∪ CLI commands ∪ data models) over the call+import graph, within an optional `scope`. On-demand graph query (no analyze-time precompute). CONSERVATIVE (fails toward `live`): reachability counts ALL edge confidence tiers (resolved ∪ ambiguous ∪ inferred), dynamic-dispatch/reflection barrier tags force their entities live, and framework-magic kinds are excluded from candidacy — so it under-reports rather than over-reports. No `confidence` argument (a ceiling would only make more code look dead). HONEST SIGNAL-UNAVAILABLE: if the current index has no root categorisation tags, the tool returns zero candidates with a missing-signal note (NOT a flood of false positives, and NOT a guarantee there is no dead code). Heuristic results (LMWV-FACT-DEAD-CODE-CANDIDATE, confidence < 1) — never certain. Bounded; SEI-carrying. No LLM call.",
            input_schema: scope_page_schema(false),
        },
        ToolDefinition {
            name: "entity_semantic_search_list",
            description: "Rank entities by semantic (embedding cosine) similarity to a `query` string, within an optional `scope`. OPT-IN: semantic search is OFF by default; when disabled or no embedding provider is configured the tool returns result_kind=`not_enabled` with a missing-signal note (never a faked or empty-as-complete result). When enabled it embeds the query and runs a bounded exact cosine scan over the git-ignored `.weft/loomweave/embeddings.db` sidecar (built at analyze time), considering only embeddings whose content_hash matches the entity's current hash (stale vectors never surface). Bounded (limit default 20, max 100; page.total/truncated). Each result carries its `sei` and a `score`.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "minLength": 1},
                    "scope": {"type": "string", "minLength": 1},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "project_finding_list",
            description: "List findings across the WHOLE project — NO entity id required — so an agent can go from project_status_get's `findings: N` count straight to the N findings (the count-without-list gap). Each row carries its anchoring entity { id, sei, file, line } plus the finding's tool/rule_id/kind/severity/status/message/confidence/created_at. Optionally filtered by `filter.kind` (defect/fact/classification/metric/suggestion), `filter.severity` (INFO/WARN/ERROR/CRITICAL/NONE), and `filter.status` (open/acknowledged/suppressed/promoted_to_issue). Bounded (limit default 50, max 200; page.total/returned/truncated). With NO filter, page.total reconciles with project_status_get's finding count (both count the bare findings table). A project with no findings returns an empty list, not an error. No LLM call.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "filter": {
                        "type": "object",
                        "properties": {
                            "kind": {"type": "string"},
                            "severity": {"type": "string"},
                            "status": {"type": "string"}
                        },
                        "additionalProperties": false
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                    "offset": {"type": "integer", "minimum": 0}
                },
                "additionalProperties": false
            }),
        },
    ]
}

#[must_use]
pub fn list_tools_for_policy(policy: McpToolPolicy) -> Vec<ToolDefinition> {
    list_tools()
        .into_iter()
        .filter(|tool| policy.allows(tool.name))
        .collect()
}

/// Input schema for the scope-aware shortcut tools: optional `scope` (entity id
/// or path glob) plus `limit`/`offset` bounds, and — when `with_since` —
/// an optional ISO-8601 `since`.
fn scope_page_schema(with_since: bool) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert(
        "scope".to_owned(),
        json!({"type": "string", "minLength": 1}),
    );
    properties.insert(
        "limit".to_owned(),
        json!({"type": "integer", "minimum": 1, "maximum": 200}),
    );
    properties.insert(
        "offset".to_owned(),
        json!({"type": "integer", "minimum": 0}),
    );
    if with_since {
        properties.insert(
            "since".to_owned(),
            json!({"type": "string", "minLength": 1}),
        );
    }
    json!({
        "type": "object",
        "properties": Value::Object(properties),
        "additionalProperties": false
    })
}

/// Input schema for a faceted-search tool: the named facet fields (each
/// `required` or not) plus the shared `scope`/`limit`/`offset` bounds.
fn scope_facet_schema(facets: &[(&str, bool)]) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (name, is_required) in facets {
        // Wardline `tier`/`group` filter against the opaque blob and accept a
        // string or number (matching the handler's `optional_facet`); other
        // facets (e.g. `tag`) are strings.
        let schema = if *name == "tier" || *name == "group" {
            json!({"type": ["string", "integer"]})
        } else {
            json!({"type": "string", "minLength": 1})
        };
        properties.insert((*name).to_owned(), schema);
        if *is_required {
            required.push(Value::String((*name).to_owned()));
        }
    }
    properties.insert(
        "scope".to_owned(),
        json!({"type": "string", "minLength": 1}),
    );
    properties.insert(
        "limit".to_owned(),
        json!({"type": "integer", "minimum": 1, "maximum": 200}),
    );
    properties.insert(
        "offset".to_owned(),
        json!({"type": "integer", "minimum": 0}),
    );
    json!({
        "type": "object",
        "properties": Value::Object(properties),
        "required": required,
        "additionalProperties": false
    })
}

/// Input schema for `entity_wardline_list`: the faceted tier/group schema plus a
/// `has_findings` boolean. Declared explicitly because the base schema sets
/// `additionalProperties: false`, which would otherwise reject the param.
fn wardline_facet_schema() -> Value {
    let mut schema = scope_facet_schema(&[("tier", false), ("group", false)]);
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.insert("has_findings".to_owned(), json!({"type": "boolean"}));
    }
    schema
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
/// [`ServerState::handle_json_rpc`] (the production `loomweave serve` path); a
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
        "initialize" => result_response(&id, &initialize_result(false, McpToolPolicy::default())),
        "tools/list" => result_response(
            &id,
            &json!({"tools": list_tools_for_policy(McpToolPolicy::default())}),
        ),
        "tools/call" => error_response(
            &id,
            -32601,
            "tools/call requires ServerState::handle_json_rpc",
        ),
        _ => error_response(&id, -32601, "method not found"),
    })
}

/// Actionable chirp for a project with no index. Mirrors the `SessionStart` hook
/// wording (`hook.rs`) so the operator sees the same "install then analyze"
/// sequence whether they read it from the shell or from an MCP client. Surfaced
/// both in the degraded `initialize` instructions and from every degraded
/// `tools/call` result.
fn no_index_message(project_root: &Path) -> String {
    let root = project_root.display();
    format!(
        "Loomweave has no index for this project yet \
({root}/.weft/loomweave/loomweave.db is missing), so the structural graph has not been \
built and every Loomweave tool is unavailable. Run `loomweave install --path {root}` \
then `loomweave analyze {root}` in a terminal to extract the entity / edge graph, \
then reconnect this MCP server."
    )
}

/// Degraded-mode orientation for the `initialize` `instructions` field. Distinct
/// from [`server_instructions`] (the healthy-index orientation) so the normal
/// path — and its `server_instructions_enumerate_every_tool` guard — is
/// untouched.
fn server_instructions_no_index(project_root: &Path) -> String {
    format!(
        "⚠ NO INDEX. {}\n\nNormally Loomweave answers \"what calls X\", \"where is X \
defined\", \"what subsystem is X in\" from a pre-extracted graph instead of grepping \
the tree — but it needs an index first. `tools/list` still shows the surface; any tool \
call returns this same instruction until the index exists.",
        no_index_message(project_root)
    )
}

/// The `initialize` result for the degraded no-index server. Advertises `tools`
/// and `prompts` (the static `loomweave-workflow` prompt works without a DB) but
/// not `resources` (the `loomweave://context` resource needs the index).
fn initialize_result_no_index(project_root: &Path) -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": {}, "prompts": {} },
        "serverInfo": {
            "name": "loomweave",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": server_instructions_no_index(project_root)
    })
}

/// JSON-RPC dispatch for the degraded "no index" stdio server: the project has
/// no `.weft/loomweave/loomweave.db`, so there is no graph to query. `initialize`
/// succeeds (the client connects cleanly rather than seeing the server die) and
/// `tools/call` returns the actionable chirp as a tool result with
/// `isError: true` — the load-bearing channel, since not every client surfaces
/// the `initialize` `instructions`. `tools/list` and the static
/// `loomweave-workflow` prompt answer normally so the surface looks healthy.
/// clarion-ac36f51c2b.
#[must_use]
pub fn handle_json_rpc_no_index(request: &Value, project_root: &Path) -> Option<Value> {
    if is_json_rpc_notification(request) {
        return None;
    }
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return Some(error_response(&id, -32600, "invalid request"));
    };

    Some(match method {
        "initialize" => result_response(&id, &initialize_result_no_index(project_root)),
        "tools/list" => result_response(
            &id,
            &json!({"tools": list_tools_for_policy(McpToolPolicy::default())}),
        ),
        "tools/call" => result_response(
            &id,
            &json!({
                "content": [
                    { "type": "text", "text": no_index_message(project_root) }
                ],
                "isError": true
            }),
        ),
        "prompts/list" => result_response(&id, &prompts_list()),
        "prompts/get" => prompts_get(&id, request.get("params")),
        _ => error_response(&id, -32601, "method not found"),
    })
}

/// Serve a degraded MCP stdio session for a project with no index. Mirrors
/// [`serve_stdio`] (synchronous — there are no storage-backed async tools to
/// drive) but routes every request through [`handle_json_rpc_no_index`]. Used by
/// `loomweave serve` when `.weft/loomweave/loomweave.db` is absent, so the client
/// connects and is told to run analyze rather than watching the server exit.
pub fn serve_stdio_no_index(
    project_root: &Path,
    reader: &mut impl std::io::BufRead,
    writer: &mut impl std::io::Write,
) -> Result<(), McpError> {
    loop {
        let Some(frame) = read_stdio_frame(reader)? else {
            return Ok(());
        };
        let framing = frame.framing;
        let request: Value = serde_json::from_slice(&frame.body)?;
        if let Some(response) = handle_json_rpc_no_index(&request, project_root) {
            write_stdio_response(writer, &encode_response_frame(&response)?, framing)?;
        }
    }
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
    /// Whether LLM summaries are enabled at all (`llm.enabled`, configured).
    pub enabled: bool,
    /// A live provider is wired and summaries will dispatch to it (effective).
    pub live: bool,
    /// Whether config permits a live provider at all (`llm.allow_live_provider`,
    /// configured).
    pub allow_live_provider: bool,
    /// Summary-cache freshness horizon in days (`llm.cache_max_age_days`).
    pub cache_max_age_days: u32,
}

#[derive(Clone)]
pub struct ServerState {
    project_root: PathBuf,
    readers: ReaderPool,
    execution_edge_cap: usize,
    execution_path_cap: usize,
    summary_llm: Option<SummaryLlmState>,
    semantic_search: Option<SemanticSearchState>,
    clock: Arc<dyn Fn() -> String + Send + Sync>,
    budget: Arc<Mutex<BudgetLedger>>,
    inferred_inflight: InferredInflight,
    filigree_client: Option<Arc<dyn FiligreeLookup>>,
    diagnostics: Option<DiagnosticsContext>,
    tool_policy: McpToolPolicy,
    /// Supervised `loomweave analyze` runs launched via `analyze_start`.
    analyze_runs: crate::analyze_runs::RunRegistry,
    active_requests: Arc<AsyncMutex<BTreeSet<String>>>,
    cancelled_requests: Arc<AsyncMutex<BTreeSet<String>>>,
    cancellation_notify: Arc<Notify>,
    /// Launcher for `analyze_start` to spawn. `None` → `current_exe()`; tests
    /// inject a stub via [`ServerState::with_analyze_command`].
    analyze_program: Option<PathBuf>,
    /// Config file the active `serve` was launched with, forwarded as
    /// `--config` to an `analyze_start`-spawned analyze so the child parses the
    /// same configuration (review #12). `None` → the child uses its default
    /// config discovery (serve was started without an explicit `--config`).
    analyze_config_path: Option<PathBuf>,
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
            semantic_search: None,
            clock: Arc::new(default_now_string),
            budget: Arc::new(Mutex::new(BudgetLedger::default())),
            inferred_inflight: Arc::new(AsyncMutex::new(HashMap::new())),
            filigree_client: None,
            diagnostics: None,
            tool_policy: McpToolPolicy::default(),
            analyze_runs: Arc::new(Mutex::new(HashMap::new())),
            active_requests: Arc::new(AsyncMutex::new(BTreeSet::new())),
            cancelled_requests: Arc::new(AsyncMutex::new(BTreeSet::new())),
            cancellation_notify: Arc::new(Notify::new()),
            analyze_program: None,
            analyze_config_path: None,
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

    /// Forward `serve`'s `--config` path to `analyze_start`-spawned analyze runs
    /// so the child parses the same configuration (review #12). Call only when
    /// serve was launched with an explicit, on-disk config file.
    #[must_use]
    pub fn with_analyze_config(mut self, config_path: PathBuf) -> Self {
        self.analyze_config_path = Some(config_path);
        self
    }

    #[must_use]
    pub fn with_tool_policy(mut self, policy: McpToolPolicy) -> Self {
        self.tool_policy = policy;
        self
    }

    /// Test-only: number of analyze run handles currently held in the registry.
    /// Lets a test assert that finished runs are evicted (clarion-7e0c21558a)
    /// rather than accumulating across a long-lived `serve`.
    #[doc(hidden)]
    #[must_use]
    pub fn tracked_analyze_runs(&self) -> usize {
        self.analyze_runs
            .lock()
            .expect("analyze run registry mutex")
            .len()
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

    /// Attach the embeddings provider + policy for `search_semantic` (`WS5b`).
    /// Absent (or `config.enabled == false`) → the tool degrades honestly to
    /// "not enabled". The provider is constructed by the caller (`serve`) only
    /// when opted in with a key present, so the trait — not the choice — is
    /// load-bearing.
    #[must_use]
    pub fn with_semantic_search(
        mut self,
        config: SemanticSearchConfig,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        self.semantic_search = Some(SemanticSearchState { config, provider });
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
            self.handle_json_rpc_notification(request).await;
            return None;
        }
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            return Some(error_response(&id, -32600, "invalid request"));
        };
        let request_key = request_id_key(&id);
        if let Some(key) = &request_key {
            self.begin_request(key).await;
        }

        let dispatch = async {
            match method {
                "initialize" => result_response(&id, &initialize_result(true, self.tool_policy)),
                "tools/list" => result_response(
                    &id,
                    &json!({"tools": list_tools_for_policy(self.tool_policy)}),
                ),
                "tools/call" => self.handle_tool_call(&id, request.get("params")).await,
                "resources/list" => result_response(&id, &resources_list()),
                "resources/read" => self.handle_resources_read(&id, request.get("params")).await,
                "prompts/list" => result_response(&id, &prompts_list()),
                "prompts/get" => prompts_get(&id, request.get("params")),
                _ => error_response(&id, -32601, "method not found"),
            }
        };

        let response = if let Some(key) = request_key.clone() {
            tokio::select! {
                response = dispatch => response,
                () = self.wait_for_cancellation(key) => {
                    error_response(&id, -32800, "request cancelled")
                }
            }
        } else {
            dispatch.await
        };

        if let Some(key) = &request_key {
            self.finish_request(key).await;
        }
        Some(response)
    }

    async fn handle_json_rpc_notification(&self, request: &Value) {
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            return;
        };
        if method != "notifications/cancelled" {
            return;
        }
        let Some(request_id) = request
            .get("params")
            .and_then(Value::as_object)
            .and_then(|params| params.get("requestId"))
            .and_then(request_id_key)
        else {
            return;
        };
        self.cancel_request(&request_id).await;
    }

    async fn begin_request(&self, request_id: &str) {
        self.active_requests
            .lock()
            .await
            .insert(request_id.to_owned());
    }

    async fn finish_request(&self, request_id: &str) {
        self.active_requests.lock().await.remove(request_id);
        self.cancelled_requests.lock().await.remove(request_id);
    }

    async fn cancel_request(&self, request_id: &str) {
        if self.active_requests.lock().await.contains(request_id) {
            self.cancelled_requests
                .lock()
                .await
                .insert(request_id.to_owned());
            self.cancellation_notify.notify_waiters();
        }
    }

    async fn wait_for_cancellation(&self, request_id: String) {
        loop {
            if self.cancelled_requests.lock().await.contains(&request_id) {
                return;
            }
            self.cancellation_notify.notified().await;
        }
    }

    // A flat dispatch table over every tool; length tracks the tool count by
    // design (mirrors the `#[allow]` on `list_tools`).
    #[allow(clippy::too_many_lines)]
    async fn handle_tool_call(&self, id: &Value, params: Option<&Value>) -> Value {
        let Some(params) = params.and_then(Value::as_object) else {
            return error_response(id, -32602, "invalid tools/call params");
        };
        let Some(name) = params.get("name").and_then(Value::as_str) else {
            return error_response(id, -32602, "invalid tools/call params: missing name");
        };
        let canonical_name = rename_old_to_new(name);
        let Some(tool) = list_tools()
            .into_iter()
            .find(|tool| tool.name == canonical_name)
        else {
            return error_response(id, -32601, &format!("unknown tool: {name}"));
        };
        if !self.tool_policy.allows(canonical_name) {
            return error_response(
                id,
                -32601,
                &format!("tool disabled by MCP tool policy: {canonical_name}"),
            );
        }
        let arguments = params.get("arguments").unwrap_or(&Value::Null);
        let Some(arguments) = arguments.as_object() else {
            return error_response(
                id,
                -32602,
                "invalid tools/call params: arguments must be object",
            );
        };
        if let Err(err) = validate_tool_arguments_against_schema(&tool, arguments) {
            return err.to_json_rpc(id);
        }
        if !self.tool_policy.allows_arguments(canonical_name, arguments) {
            return error_response(
                id,
                -32602,
                "confidence=inferred/all is disabled by MCP tool policy because it may call an LLM and write inferred-edge cache rows",
            );
        }

        let envelope = match canonical_name {
            "entity_at" => match self.tool_entity_at(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_find" => match self.tool_find_entity(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_callers_list" => match self.tool_callers_of(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_execution_path_list" => match self.tool_execution_paths_from(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_neighborhood_get" => match self.tool_neighborhood(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_summary_get" => match self.tool_summary(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_issue_list" => match self.tool_issues_for(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "subsystem_member_list" => match self.tool_subsystem_members(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_subsystem_get" => match self.tool_subsystem_of(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "project_status_get" => match self.tool_project_status(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_summary_preview_cost_get" => {
                match self.tool_summary_preview_cost(arguments).await {
                    Ok(value) => value,
                    Err(response) => return response.to_json_rpc(id),
                }
            }
            "entity_source_get" => match self.tool_source_for_entity(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_call_site_list" => match self.tool_call_sites(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_orientation_pack_get" => match self.tool_orientation_pack(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "analyze_start" => match self.tool_analyze_start(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "analyze_status_get" => match self.tool_analyze_status(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "analyze_cancel" => match self.tool_analyze_cancel(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "index_diff_get" => match self.tool_index_diff(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_guidance_list" => match self.tool_guidance_for(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "propose_guidance" => match self.tool_propose_guidance(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "promote_guidance" => match self.tool_promote_guidance(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_finding_list" => match self.tool_findings_for(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_wardline_get" => match self.tool_wardline_for(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_tag_list" => match self.tool_find_by_tag(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_kind_list" => match self.tool_find_by_kind(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_wardline_list" => match self.tool_find_by_wardline(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "module_circular_import_list" => match self.tool_find_circular_imports(arguments).await
            {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_coupling_hotspot_list" => {
                match self.tool_find_coupling_hotspots(arguments).await {
                    Ok(value) => value,
                    Err(response) => return response.to_json_rpc(id),
                }
            }
            "entity_entry_point_list" => match self.tool_find_entry_points(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_http_route_list" => match self.tool_find_http_routes(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_data_model_list" => match self.tool_find_data_models(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_test_list" => match self.tool_find_tests(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_deprecation_list" => match self.tool_find_deprecations(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_todo_list" => match self.tool_find_todos(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_test_caller_list" => match self.tool_what_tests_this(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_high_churn_list" => match self.tool_high_churn(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_recent_change_list" => match self.tool_recently_changed(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_dead_list" => match self.tool_find_dead_code(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "entity_semantic_search_list" => match self.tool_search_semantic(arguments).await {
                Ok(value) => value,
                Err(response) => return response.to_json_rpc(id),
            },
            "project_finding_list" => match self.tool_project_findings(arguments).await {
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
        if uri != "loomweave://context" {
            return error_response(id, -32602, &format!("unknown resource: {uri}"));
        }
        let snapshot_json = self.context_snapshot_json().await;
        result_response(
            id,
            &json!({
                "contents": [
                    {
                        "uri": "loomweave://context",
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
                tracing::warn!(error = %err, "loomweave://context snapshot failed");
                fallback()
            }
        }
    }

    async fn tool_propose_guidance(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let entity_id = required_str(arguments, "entity_id")?.to_owned();
        let content = required_str(arguments, "content")?.to_owned();
        let scope_level = arguments
            .get("scope_level")
            .and_then(Value::as_str)
            .unwrap_or("function")
            .to_owned();
        let pinned = optional_bool(arguments, "pinned")?.unwrap_or(false);
        let name = arguments
            .get("name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let expires = arguments
            .get("expires")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let match_rules = arguments
            .get("match_rules")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| vec![json!({"type": "entity", "id": entity_id})]);

        let project_root = self.project_root.clone();
        let entity_lookup_id = entity_id.clone();
        let entity = match self
            .readers
            .with_reader(move |conn| entity_by_id(conn, &entity_lookup_id))
            .await
        {
            Ok(Some(entity)) => entity,
            Ok(None) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::EntityNotFound,
                    &format!("entity {entity_id} was not found"),
                    false,
                ));
            }
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &format!("read entity for guidance proposal: {err}"),
                    storage_retryable(&err),
                ));
            }
        };

        let proposal = GuidanceProposal {
            entity_id: entity_id.clone(),
            content,
            scope_level,
            match_rules,
            name,
            pinned,
            expires,
        };
        let detail = match proposal.to_observation_detail() {
            Ok(detail) => detail,
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &format!("build guidance proposal: {err}"),
                    false,
                ));
            }
        };

        let Some(client) = self.filigree_client.clone() else {
            return Ok(tool_error_envelope(
                McpErrorCode::IoError,
                "Filigree integration is not configured; cannot create guidance proposal observation",
                true,
            ));
        };
        let file_path = entity.source_file_path.as_deref().map(|path| {
            std::path::Path::new(path)
                .strip_prefix(&project_root)
                .ok()
                .and_then(|rel| rel.to_str())
                .unwrap_or(path)
                .to_owned()
        });
        let request = ObservationCreateRequest {
            summary: format!("Loomweave guidance proposal for {entity_id}"),
            detail,
            file_path,
            line: entity.source_line_start,
            priority: 2,
            actor: "loomweave".to_owned(),
        };

        let response =
            match tokio::task::spawn_blocking(move || client.create_observation(request)).await {
                Ok(Ok(response)) => response,
                Ok(Err(err)) => {
                    return Ok(tool_error_envelope(
                        McpErrorCode::IoError,
                        &format!("create Filigree guidance proposal observation: {err}"),
                        true,
                    ));
                }
                Err(err) => {
                    return Ok(tool_error_envelope(
                        McpErrorCode::Internal,
                        &format!("create Filigree guidance proposal task failed: {err}"),
                        true,
                    ));
                }
            };

        Ok(success_envelope(json!({
            "observation_id": response.observation_id,
            "promoted": false,
        })))
    }

    async fn tool_promote_guidance(
        &self,
        arguments: &serde_json::Map<String, Value>,
    ) -> std::result::Result<Value, ParamError> {
        let observation_id = required_str(arguments, "observation_id")?.to_owned();
        let Some(client) = self.filigree_client.clone() else {
            return Ok(tool_error_envelope(
                McpErrorCode::IoError,
                "Filigree integration is not configured; cannot read guidance proposal observation",
                true,
            ));
        };
        let lookup_client = client.clone();
        let lookup_id = observation_id.clone();
        let observation =
            match tokio::task::spawn_blocking(move || lookup_client.observation_by_id(&lookup_id))
                .await
            {
                Ok(Ok(Some(observation))) => observation,
                Ok(Ok(None)) => {
                    return Ok(tool_error_envelope(
                        McpErrorCode::NotFound,
                        &format!("observation {observation_id} was not found"),
                        false,
                    ));
                }
                Ok(Err(err)) => {
                    return Ok(tool_error_envelope(
                        McpErrorCode::IoError,
                        &format!("read Filigree observation {observation_id}: {err}"),
                        true,
                    ));
                }
                Err(err) => {
                    return Ok(tool_error_envelope(
                        McpErrorCode::Internal,
                        &format!("read Filigree observation task failed: {err}"),
                        true,
                    ));
                }
            };

        let proposal = match GuidanceProposal::from_observation_detail(&observation.detail) {
            Ok(proposal) => proposal,
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &format!("observation {observation_id} is not a guidance proposal: {err}"),
                    false,
                ));
            }
        };
        let authored_at = guidance_authored_at_from_clock(&(self.clock)());
        let promoted = match proposal.to_promoted_sheet(&authored_at) {
            Ok(promoted) => promoted,
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &format!("build promoted guidance sheet: {err}"),
                    false,
                ));
            }
        };

        let db_path = loomweave_core::store::db_path(&self.project_root);
        let project_root = self.project_root.clone();
        let sheet_id = promoted.id.clone();
        let write_result =
            tokio::task::spawn_blocking(move || -> std::result::Result<usize, String> {
                let conn =
                    open_guidance_write_connection(&db_path).map_err(|err| err.to_string())?;
                upsert_guidance_sheet(
                    &conn,
                    &GuidanceSheetInput {
                        id: &promoted.id,
                        name: &promoted.name,
                        short_name: &promoted.short_name,
                        properties: &promoted.properties,
                    },
                )
                .map_err(|err| err.to_string())?;
                let Some(sheet) = loomweave_storage::get_guidance_sheet(&conn, &promoted.id)
                    .map_err(|err| err.to_string())?
                else {
                    return Ok(0);
                };
                invalidate_summaries_for_sheet(&conn, &sheet, &project_root)
                    .map_err(|err| err.to_string())
            })
            .await;
        let invalidated = match write_result {
            Ok(Ok(invalidated)) => invalidated,
            Ok(Err(err)) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::StorageError,
                    &format!("write promoted guidance sheet {sheet_id}: {err}"),
                    true,
                ));
            }
            Err(err) => {
                return Ok(tool_error_envelope(
                    McpErrorCode::Internal,
                    &format!("write promoted guidance sheet task failed: {err}"),
                    true,
                ));
            }
        };

        let dismiss_id = observation_id.clone();
        let dismissed = tokio::task::spawn_blocking(move || {
            client.dismiss_observation(&dismiss_id, "promoted to Loomweave guidance sheet")
        })
        .await
        .is_ok_and(|result| result.is_ok());

        Ok(success_envelope(json!({
            "observation_id": observation_id,
            "sheet_id": sheet_id,
            "invalidated_summaries": invalidated,
            "observation_dismissed": dismissed,
        })))
    }
}

async fn invoke_llm_provider(
    provider: Arc<dyn LlmProvider>,
    request: LlmRequest,
) -> Result<LlmResponse, LlmProviderError> {
    provider.invoke(request).await
}

fn open_guidance_write_connection(path: &std::path::Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

#[derive(Clone)]
struct SummaryLlmState {
    writer: mpsc::Sender<WriterCmd>,
    config: LlmConfig,
    provider: Arc<dyn LlmProvider>,
}

#[derive(Clone)]
struct SemanticSearchState {
    config: SemanticSearchConfig,
    provider: Arc<dyn EmbeddingProvider>,
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
    // The pre-serialized entity payload (carrying its `sei`) is built in the
    // reader closure; the summary envelope is otherwise assembled after async
    // writer/LLM calls where no reader connection is in scope (REQ-C-04 /
    // ADR-038).
    ScopeDeferred(Value),
    BriefingBlocked(Value, String),
}

struct SummaryReady {
    entity: EntityRow,
    /// Pre-serialized entity payload (including the SEI read-time join), built
    /// in the reader closure for the same reason as the variants above.
    entity_json: Value,
    key: SummaryCacheKey,
    cached: Option<SummaryCacheEntry>,
    guidance_text: String,
    caller_count: i64,
    fan_out: i64,
}

struct IssuesForRead {
    entities: Vec<EntityRow>,
    /// Pre-serialized entity payloads keyed by locator, built inside the reader
    /// closure so the SEI read-time join (REQ-C-04 / ADR-038) happens while a
    /// connection is in scope. `tool_issues_for` runs outside any reader closure
    /// (it interleaves Filigree HTTP calls), so the `sei` field cannot be
    /// resolved during accumulation — it is resolved here, ahead of the loop.
    entity_json_by_id: HashMap<String, Value>,
    entity_cap_truncated: bool,
}

struct IssuesForAccumulator {
    entities_by_id: HashMap<String, EntityRow>,
    entity_json_by_id: HashMap<String, Value>,
    association_aliases: HashMap<String, String>,
    seen_issue_ids: BTreeSet<String>,
    matched: Vec<Value>,
    drifted: Vec<Value>,
    not_found: Vec<Value>,
    diagnostics: Vec<Value>,
    emitted: usize,
    issue_cap_truncated: bool,
}

impl IssuesForAccumulator {
    fn new(entities: &[EntityRow], entity_json_by_id: HashMap<String, Value>) -> Self {
        // Map every key Filigree might echo back in `loomweave_entity_id` to the
        // current locator (`entity.id`). A SEI-bearing entity is queried by SEI
        // only (see `tool_issues_for`), so the SEI→locator alias is the live
        // path for those rows; the locator self-mapping covers no-SEI entities
        // and any straggler locator-keyed rows during the SEI migration window.
        let mut association_aliases = HashMap::new();
        for entity in entities {
            association_aliases.insert(entity.id.clone(), entity.id.clone());
            if let Some(sei) = entity_json_by_id
                .get(&entity.id)
                .and_then(|json| json.get("sei"))
                .and_then(Value::as_str)
                .filter(|sei| !sei.trim().is_empty())
            {
                association_aliases.insert(sei.to_owned(), entity.id.clone());
            }
        }
        Self {
            entities_by_id: entities
                .iter()
                .map(|entity| (entity.id.clone(), entity.clone()))
                .collect(),
            entity_json_by_id,
            association_aliases,
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
        // The nested entity payload (including its `sei`) was pre-built in the
        // reader closure; `entities_by_id` is retained only for content-hash
        // drift classification.
        let canonical_entity_id = self
            .association_aliases
            .get(&association.loomweave_entity_id)
            .map_or(association.loomweave_entity_id.as_str(), String::as_str);
        let entity_json = self.entity_json_by_id.get(canonical_entity_id).cloned();
        match self.entities_by_id.get(canonical_entity_id) {
            None => {
                self.not_found
                    .push(association_json(association, None, None, "not_found", None));
            }
            Some(entity) => match entity.content_hash.as_deref() {
                Some(current_hash) if current_hash == association.content_hash_at_attach => {
                    self.matched.push(association_json(
                        association,
                        entity_json.as_ref(),
                        Some(current_hash),
                        "matched",
                        Some(&entity.id),
                    ));
                }
                Some(current_hash) => {
                    self.drifted.push(association_json(
                        association,
                        entity_json.as_ref(),
                        Some(current_hash),
                        "drifted",
                        Some(&entity.id),
                    ));
                }
                None => {
                    self.diagnostics.push(json!({
                        "code": "LMWV-ENTITY-CONTENT-HASH-MISSING",
                        "entity_id": entity.id
                    }));
                    self.matched.push(association_json(
                        association,
                        entity_json.as_ref(),
                        None,
                        "unknown",
                        Some(&entity.id),
                    ));
                }
            },
        }
    }

    /// Unique `issue_id`s across matched + drifted entries, in first-seen
    /// order. `add_response` already dedupes `issue_id`s globally, so this is
    /// the set of distinct issues to fetch detail for — one request each (no
    /// N+1).
    fn enrichable_issue_ids(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut ids = Vec::new();
        for entry in self.matched.iter().chain(self.drifted.iter()) {
            if let Some(id) = entry.get("issue_id").and_then(Value::as_str)
                && seen.insert(id.to_owned())
            {
                ids.push(id.to_owned());
            }
        }
        ids
    }

    /// Attach an `issue` field (title/status/priority) to every matched and
    /// drifted entry. The value is the fetched [`IssueDetail`] when available,
    /// else `null` — a stable shape that signals "enrichment attempted, no
    /// detail" without forcing the consumer to probe for a missing key.
    fn apply_issue_details(&mut self, details: &HashMap<String, Option<IssueDetail>>) {
        for entry in self.matched.iter_mut().chain(self.drifted.iter_mut()) {
            let issue_value = entry
                .get("issue_id")
                .and_then(Value::as_str)
                .and_then(|id| details.get(id))
                .and_then(Option::as_ref)
                .and_then(|detail| serde_json::to_value(detail).ok())
                .unwrap_or(Value::Null);
            if let Some(object) = entry.as_object_mut() {
                object.insert("issue".to_owned(), issue_value);
            }
        }
    }

    fn into_envelope(
        self,
        entity_cap_truncated: bool,
        requests_total: usize,
        detail_requests_total: usize,
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
                "filigree_issues_returned_total": self.emitted,
                "filigree_detail_requests_total": detail_requests_total
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
    code: McpErrorCode,
    message: String,
    retryable: bool,
    stats_delta: Value,
    diagnostics: Vec<Value>,
}

impl InferredDispatchFailure {
    fn new(code: McpErrorCode, message: &str, retryable: bool) -> Self {
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
            code: McpErrorCode::StorageError,
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
        if self.code == McpErrorCode::TokenCeilingExceeded {
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
    handle_request_value_with_state(state, request).await
}

async fn handle_request_value_with_state(
    state: &ServerState,
    request: Value,
) -> Result<Option<Frame>, McpError> {
    let Some(response) = state.handle_json_rpc(&request).await else {
        return Ok(None);
    };
    Ok(Some(encode_response_frame(&response)?))
}

fn handle_stdio_frame(frame: &Frame) -> Result<Option<Frame>, McpError> {
    handle_frame(frame)
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

fn request_id_key(id: &Value) -> Option<String> {
    match id {
        Value::String(value) => Some(format!("s:{value}")),
        Value::Number(value) => Some(format!("n:{value}")),
        _ => None,
    }
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
    match loomweave_core::plugin::read_frame(reader, ContentLengthCeiling::DEFAULT) {
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
            loomweave_core::plugin::write_frame(writer, response)?;
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
    reader: &mut (impl std::io::BufRead + Send),
    writer: &mut impl std::io::Write,
) -> Result<(), McpError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    serve_stdio_with_state_on_runtime(&runtime, state, reader, writer)
}

pub fn serve_stdio_with_state_on_runtime(
    runtime: &tokio::runtime::Runtime,
    state: &ServerState,
    reader: &mut (impl std::io::BufRead + Send),
    writer: &mut impl std::io::Write,
) -> Result<(), McpError> {
    let _guard = runtime.enter();
    let (frame_tx, frame_rx) = mpsc::unbounded_channel();
    let state = state.clone();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            loop {
                let message = read_stdio_frame(reader);
                let done = !matches!(message, Ok(Some(_)));
                if frame_tx.send(message).is_err() || done {
                    break;
                }
            }
        });
        runtime.block_on(serve_stdio_with_state_event_loop(state, frame_rx, writer))
    })
}

async fn serve_stdio_with_state_event_loop(
    state: ServerState,
    mut frame_rx: mpsc::UnboundedReceiver<Result<Option<StdioFrame>, McpError>>,
    writer: &mut impl std::io::Write,
) -> Result<(), McpError> {
    let (response_tx, mut response_rx) =
        mpsc::unbounded_channel::<(StdioFraming, Result<Option<Frame>, McpError>)>();
    let mut input_closed = false;
    let mut pending_responses = 0usize;

    loop {
        tokio::select! {
            maybe_frame = frame_rx.recv(), if !input_closed => {
                match maybe_frame {
                    Some(Ok(Some(frame))) => {
                        let request: Value = serde_json::from_slice(&frame.body)?;
                        if should_spawn_stateful_stdio_request(&request) {
                            if let Some(key) = request.get("id").and_then(request_id_key) {
                                state.begin_request(&key).await;
                            }
                            pending_responses += 1;
                            let task_state = state.clone();
                            let task_tx = response_tx.clone();
                            let framing = frame.framing;
                            tokio::spawn(async move {
                                let result = handle_request_value_with_state(&task_state, request).await;
                                let _ = task_tx.send((framing, result));
                            });
                        } else if let Some(response) = handle_request_value_with_state(&state, request).await? {
                            write_stdio_response(writer, &response, frame.framing)?;
                        }
                    }
                    Some(Ok(None)) | None => {
                        input_closed = true;
                    }
                    Some(Err(err)) => return Err(err),
                }
            }
            maybe_response = response_rx.recv(), if pending_responses > 0 => {
                let Some((framing, result)) = maybe_response else {
                    return Err(McpError::Runtime(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "MCP stdio response channel closed with pending requests",
                    )));
                };
                pending_responses = pending_responses.saturating_sub(1);
                if let Some(response) = result? {
                    write_stdio_response(writer, &response, framing)?;
                }
            }
            else => {
                if input_closed && pending_responses == 0 {
                    return Ok(());
                }
            }
        }
    }
}

fn should_spawn_stateful_stdio_request(request: &Value) -> bool {
    request
        .as_object()
        .is_some_and(|object| object.get("id").is_some())
        && request
            .get("method")
            .and_then(Value::as_str)
            .is_some_and(|method| method == "tools/call")
}

/// Build the `initialize` result, advertising only the capabilities the
/// handling path actually serves. The stateless free [`handle_json_rpc`] serves
/// `tools` only (it returns method-not-found for `resources/*` and `prompts/*`),
/// so it passes `stateful = false`; [`ServerState::handle_json_rpc`] serves the
/// full surface and passes `stateful = true`. The `instructions` field is static
/// orientation guidance (not a capability) and is included in both.
fn initialize_result(stateful: bool, policy: McpToolPolicy) -> Value {
    let capabilities = if stateful {
        json!({ "tools": {}, "prompts": {}, "resources": {} })
    } else {
        json!({ "tools": {} })
    };
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": capabilities,
        "serverInfo": {
            "name": "loomweave",
            "version": env!("CARGO_PKG_VERSION")
        },
        "instructions": server_instructions(policy)
    })
}

fn resources_list() -> Value {
    json!({
        "resources": [
            {
                "uri": "loomweave://context",
                "name": "Loomweave project context",
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
                "name": "loomweave-workflow",
                "description": "How to use Loomweave's MCP tools to navigate this codebase."
            }
        ]
    })
}

fn prompts_get(id: &Value, params: Option<&Value>) -> Value {
    let name = params
        .and_then(Value::as_object)
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str);
    if name != Some("loomweave-workflow") {
        return error_response(id, -32602, "unknown prompt");
    }
    result_response(
        id,
        &json!({
            "description": "How to use Loomweave's MCP tools to navigate this codebase.",
            "messages": [
                {
                    "role": "user",
                    "content": { "type": "text", "text": LOOMWEAVE_WORKFLOW_SKILL }
                }
            ]
        }),
    )
}

fn validate_tool_arguments_against_schema(
    tool: &ToolDefinition,
    arguments: &serde_json::Map<String, Value>,
) -> std::result::Result<(), ParamError> {
    if tool
        .input_schema
        .get("additionalProperties")
        .and_then(Value::as_bool)
        != Some(false)
    {
        return Ok(());
    }
    let Some(properties) = tool
        .input_schema
        .get("properties")
        .and_then(Value::as_object)
    else {
        return Ok(());
    };
    for key in arguments.keys() {
        if !properties.contains_key(key) {
            return Err(ParamError::new(&format!(
                "unknown argument for {}: {key}",
                tool.name
            )));
        }
    }
    Ok(())
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
    let Some(value) = arguments.get("confidence") else {
        return Ok(EdgeConfidence::Resolved);
    };
    match value.as_str() {
        Some("resolved") => Ok(EdgeConfidence::Resolved),
        Some("ambiguous") => Ok(EdgeConfidence::Ambiguous),
        Some("inferred") => Ok(EdgeConfidence::Inferred),
        Some(_) => Err(ParamError::new(
            "confidence must be one of resolved, ambiguous, inferred",
        )),
        None => Err(ParamError::new("confidence must be a string")),
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
        Err(err) => tool_error_envelope(
            McpErrorCode::StorageError,
            &err.to_string(),
            storage_retryable(&err),
        ),
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
        "SELECT id, status, started_at, completed_at, owner_pid, heartbeat_at, \
            analyzed_at_commit FROM runs \
         ORDER BY started_at DESC LIMIT 1",
        [],
        |row| {
            Ok(json!({
                "id": row.get::<_, String>(0)?,
                "status": row.get::<_, String>(1)?,
                "started_at": row.get::<_, String>(2)?,
                "completed_at": row.get::<_, Option<String>>(3)?,
                "owner_pid": row.get::<_, Option<i64>>(4)?,
                "heartbeat_at": row.get::<_, Option<String>>(5)?,
                "analyzed_at_commit": row.get::<_, Option<String>>(6)?,
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
        Err(err) => tool_error_envelope(
            McpErrorCode::StorageError,
            &err.to_string(),
            storage_retryable(&err),
        ),
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

fn tool_error_envelope(code: McpErrorCode, message: &str, retryable: bool) -> Value {
    tool_error_envelope_with_diagnostics(code, message, retryable, json!({}), Vec::new())
}

fn tool_error_envelope_with_diagnostics(
    code: McpErrorCode,
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
            "code": code.as_str(),
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
            "code": McpErrorCode::TokenCeilingExceeded.as_str(),
            "message": message,
            "retryable": false
        },
        "diagnostics": [
            {
                "code": "LMWV-LLM-TOKEN-CEILING-EXCEEDED",
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

/// `reason` is a degraded-result discriminant for the `issues_for` tool
/// (`filigree-disabled`, `entity-not-found`, `filigree-unreachable`,
/// `filigree-client-error`), NOT a member of the `McpErrorCode` error-code
/// vocabulary. It lives on a `success_envelope` (`available: false`), not an
/// error envelope. `entity-not-found` here coincidentally matches
/// `McpErrorCode::EntityNotFound`'s wire spelling but is intentionally a bare
/// string: most of this closed set are not error codes, and this surface has
/// its own consumers, so the two axes are kept independent (see ADR-037).
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

/// The degraded `wardline_findings` section returned when the findings cannot
/// be fetched (transport/HTTP error) or the blocking task panicked. Single
/// source of truth for the four-key `unavailable` shape.
fn wardline_unavailable(reason: &str) -> Value {
    serde_json::json!({
        "result_kind": "unavailable",
        "items": [],
        "omitted_no_qualname": 0,
        "reason": reason,
    })
}

/// Build the `wardline_findings` enrich section for one entity. Enrich-only:
/// a fetch error degrades to `result_kind: "unavailable"` rather than failing
/// the tool.
fn wardline_section_for_entity(
    client: &std::sync::Arc<dyn crate::filigree::FiligreeLookup>,
    project_root: &Path,
    entity_id: &str,
    source_file_path: Option<&str>,
) -> Value {
    let Some(path) = source_file_path else {
        return serde_json::json!({ "result_kind": "no_matches", "items": [], "omitted_no_qualname": 0 });
    };
    let path = match project_relative_lookup_path(project_root, path) {
        Ok(path) => path,
        Err(err) => {
            return wardline_unavailable(&format!(
                "cannot normalize source_file_path for Filigree lookup: {err}"
            ));
        }
    };
    match client.wardline_findings_for_path(&path) {
        Ok(findings) => {
            let result = crate::wardline_reconcile::reconcile_for_entity(entity_id, findings);
            let items: Vec<Value> = result
                .matched
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "rule_id": m.finding.rule_id,
                        "message": m.finding.message,
                        "severity": m.finding.severity,
                        "status": m.finding.status,
                        "line_start": m.finding.line_start,
                        "line_end": m.finding.line_end,
                        "fingerprint": m.finding.fingerprint,
                        "resolution_confidence": m.resolution_confidence,
                        "wardline": m.finding.metadata.get("wardline").cloned().unwrap_or(Value::Null),
                    })
                })
                .collect();
            let result_kind = if items.is_empty() {
                "no_matches"
            } else {
                "matched"
            };
            serde_json::json!({
                "result_kind": result_kind,
                "items": items,
                "omitted_no_qualname": result.omitted_no_qualname,
            })
        }
        Err(err) => wardline_unavailable(&err.to_string()),
    }
}

fn project_relative_lookup_path(
    project_root: &Path,
    source_file_path: &str,
) -> Result<String, String> {
    let root = project_root
        .canonicalize()
        .map_err(|err| format!("canonicalize project root: {err}"))?;
    let input = Path::new(source_file_path);
    let absolute = if input.is_absolute() {
        normalize_path_lexically(input)
    } else {
        normalize_path_lexically(&root.join(input))
    };
    if !absolute.starts_with(&root) {
        return Err(format!(
            "{source_file_path:?} escapes project root {}",
            root.display()
        ));
    }
    let relative = absolute
        .strip_prefix(&root)
        .map_err(|err| format!("strip project root: {err}"))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Err(format!("{source_file_path:?} is not valid UTF-8"));
                };
                parts.push(part.to_owned());
            }
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "{source_file_path:?} is not a clean project-relative path"
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(format!("{source_file_path:?} does not name a file path"));
    }
    Ok(parts.join("/"))
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn association_json(
    association: &EntityAssociation,
    entity: Option<&Value>,
    current_content_hash: Option<&str>,
    drift_status: &str,
    canonical_entity_id: Option<&str>,
) -> Value {
    let entity_id = canonical_entity_id.unwrap_or(&association.loomweave_entity_id);
    let mut value = json!({
        "issue_id": association.issue_id,
        "entity_id": entity_id,
        "entity": entity,
        "content_hash_at_attach": association.content_hash_at_attach,
        "current_content_hash": current_content_hash,
        "attached_at": association.attached_at,
        "attached_by": association.attached_by,
        "drift_status": drift_status
    });
    if entity_id != association.loomweave_entity_id
        && let Some(object) = value.as_object_mut()
    {
        object.insert(
            "association_entity_id".to_owned(),
            json!(association.loomweave_entity_id),
        );
    }
    value
}

fn summary_read_error(read: SummaryRead) -> Value {
    match read {
        SummaryRead::EntityNotFound(id) => tool_error_envelope(
            McpErrorCode::EntityNotFound,
            &format!("entity {id} was not found"),
            false,
        ),
        SummaryRead::MissingContentHash(id) => tool_error_envelope(
            McpErrorCode::ContentHashMissing,
            &format!("entity {id} has no content hash for summary cache keying"),
            false,
        ),
        SummaryRead::ScopeDeferred(entity_json) => summary_scope_deferred(&entity_json),
        SummaryRead::BriefingBlocked(entity_json, reason) => {
            summary_briefing_blocked(&entity_json, &reason)
        }
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
            "entity {} source content drifted: stored content_hash {} but current file hashes to {}; rerun `loomweave analyze` before requesting LLM output",
            self.entity_id, self.stored_content_hash, self.current_content_hash
        )
    }

    fn to_envelope(&self) -> Value {
        tool_error_envelope(McpErrorCode::ContentDrift, &self.message(), false)
    }

    fn to_inferred_failure(&self) -> InferredDispatchFailure {
        InferredDispatchFailure::new(McpErrorCode::ContentDrift, &self.message(), false)
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
    entity_json: &Value,
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
            "entity": entity_json,
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

fn summary_scope_deferred(entity_json: &Value) -> Value {
    success_envelope(json!({
        "available": false,
        "reason": "summary-scope-deferred",
        "message": "subsystem summaries are deferred to v0.2",
        "entity": entity_json
    }))
}

fn summary_briefing_blocked(entity_json: &Value, reason: &str) -> Value {
    let remediation = briefing_block_remediation(reason);
    let entity_id = entity_json
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    success_envelope(json!({
        "available": false,
        "entity_id": entity_id,
        "entity": entity_json,
        "summary": null,
        "briefing_blocked": reason,
        "remediation": remediation
    }))
}

fn briefing_block_reason(entity: &EntityRow) -> Option<String> {
    entity_briefing_block_reason(&entity.properties_json)
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

/// Serialize an entity's stable identity fields, without the SEI.
///
/// This is the conn-free core used both by [`entity_json`] (which adds the SEI
/// for client-facing tool responses) and by internal payloads — notably the LLM
/// inference prompt — that must *not* gain a `sei` field (it is neither a tool
/// return surface nor allowed to change shape; see REQ-C-04 scope).
fn entity_identity_json(entity: &EntityRow) -> Value {
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

/// Serialize an entity for an MCP tool response.
///
/// Per REQ-C-04 / ADR-038, every tool surface that returns an entity `id` must
/// also carry its SEI: the `id` (locator) is the *mutable* address that changes
/// on rename/move, while the `sei` is the durable cross-tool binding key. The
/// SEI is read via a read-time join (`sei_for_locator`) and graceful-degrades
/// to JSON `null` on a pre-SEI database or an orphaned/unbound locator — the
/// lookup must never fail the tool call.
fn entity_json(conn: &rusqlite::Connection, entity: &EntityRow) -> Value {
    // A secret-scan-blocked entity (ADR-013) must not have its identity disclosed
    // by a discovery/structure MCP read — matching the federation read API, whose
    // BRIEFING_BLOCKED response omits id/name/path/hash (ADR-034). This is the
    // single choke point: every list/structure surface projects entities (and
    // their caller/callee/reference/import neighbors) through here
    // (clarion-307668e2be). The deliberate exception — `summary`, which echoes a
    // caller-named entity's identity + remediation — builds identity via
    // `entity_identity_json` instead, bypassing this gate.
    if let Some(reason) = briefing_block_reason(entity) {
        return blocked_entity_stub(&reason);
    }
    let mut value = entity_identity_json(entity);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "sei".to_owned(),
            json!(sei_for_locator(conn, &entity.id).ok().flatten()),
        );
    }
    value
}

/// The identity projection of a briefing-blocked entity (ADR-013 secret scan).
///
/// Every identity field is withheld — only the block reason remains — so a
/// discovery/structure MCP read acknowledges the entity exists without
/// disclosing its name, path, or line span. Mirrors the federation read API,
/// whose `BRIEFING_BLOCKED` response omits the same fields (ADR-034). The
/// qualname-bearing `id` is nulled too: the locator itself encodes the name.
fn blocked_entity_stub(reason: &str) -> Value {
    json!({
        "id": Value::Null,
        "sei": Value::Null,
        "kind": Value::Null,
        "name": Value::Null,
        "short_name": Value::Null,
        "source_file_path": Value::Null,
        "source_line_start": Value::Null,
        "source_line_end": Value::Null,
        "content_hash": Value::Null,
        "briefing_blocked": reason,
    })
}

/// Placeholder substituted for a briefing-blocked entity's id in execution-path
/// arrays. Distinct blocked nodes collapse to this one token (uncorrelatable by
/// design — the accepted price of withholding identity, clarion-307668e2be).
const BRIEFING_BLOCKED_PATH_SENTINEL: &str = "[briefing-blocked]";

/// Operator-facing remediation for a briefing block, by reason. Shared by every
/// refusal envelope (summary / neighborhood / orientation) so the "fix the
/// secret" guidance stays consistent.
fn briefing_block_remediation(reason: &str) -> &'static str {
    if reason == "unscanned_source" {
        "Entity source file was not covered by the pre-ingest secret scan. Re-run with scanner coverage for that path or fix the plugin source path before requesting a summary."
    } else {
        "File flagged by pre-ingest secret scan. Fix the secret or whitelist via .weft/loomweave/secrets-baseline.yaml. See ADR-013."
    }
}

/// Refusal envelope for a structure-fan-out read (`neighborhood`,
/// `orientation`) whose queried entity is itself briefing-blocked. Withholds the
/// structure *around* the withheld entity (ADR-034) and discloses no identity.
fn blocked_entity_refusal(reason: &str) -> Value {
    success_envelope(json!({
        "available": false,
        "briefing_blocked": reason,
        "remediation": briefing_block_remediation(reason),
    }))
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
/// declaration/signature, the body, or merely a containing scope (file-scope
/// plugin entity, or an entity without recorded sub-ranges). Honest by
/// construction — a blank or comment line that only a file-scope entity spans
/// reports `containing_range`, never a fabricated exact match
/// (clarion-460def6a51 acceptance #3).
fn match_reason_for(line: i64, entity: &EntityRow) -> &'static str {
    if is_plugin_file_scope_entity(entity) {
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
fn stack_entity_json(conn: &rusqlite::Connection, entity: &EntityRow) -> Value {
    // A blocked entity in the containing stack (the matched node, or a blocked
    // ancestor module) is redacted to a stub — same identity-withholding as
    // `entity_json` (clarion-307668e2be).
    if let Some(reason) = briefing_block_reason(entity) {
        return json!({
            "id": Value::Null,
            "sei": Value::Null,
            "kind": Value::Null,
            "short_name": Value::Null,
            "name": Value::Null,
            "source_line_start": Value::Null,
            "source_line_end": Value::Null,
            "briefing_blocked": reason,
        });
    }
    // REQ-C-04: a containing-stack ancestor is sometimes the ONLY place an
    // entity appears in the response, so it carries its `sei` (the durable
    // binding key) — not just the mutable `id`/locator. Graceful-degrade null.
    json!({
        "id": entity.id,
        "sei": sei_for_locator(conn, &entity.id).ok().flatten(),
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
    conn: &rusqlite::Connection,
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
        "scan_truncated": snapshot.scan_truncated(),
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
    let mut containing_stack: Vec<Value> = ancestors
        .iter()
        .rev()
        .map(|e| stack_entity_json(conn, e))
        .collect();
    containing_stack.push(stack_entity_json(conn, matched));

    // A blocked matched entity withholds its line span too — the ranges block
    // would otherwise disclose exactly what the block hides (clarion-307668e2be).
    let ranges = if let Some(reason) = briefing_block_reason(matched) {
        json!({
            "source_line_start": Value::Null,
            "source_line_end": Value::Null,
            "decl_line": Value::Null,
            "body_line_start": Value::Null,
            "decorator_line_start": Value::Null,
            "decorator_line_end": Value::Null,
            "briefing_blocked": reason,
        })
    } else {
        let def = DefinitionSpan::from_entity(matched);
        json!({
            "source_line_start": matched.source_line_start,
            "source_line_end": matched.source_line_end,
            "decl_line": def.decl_line,
            "body_line_start": def.body_line_start,
            "decorator_line_start": def.decorator_line_start,
            "decorator_line_end": def.decorator_line_end,
        })
    };

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
                    "entity": entity_json(conn, cand),
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
    /// Whether the served index has any alive SEI bindings (REQ-C-04 /
    /// ADR-038). Resolved in the reader closure and surfaced under `health.sei`.
    sei_populated: bool,
    neighbors_omitted: serde_json::Map<String, Value>,
    paths_truncation_reason: Option<String>,
    /// Set when the resolved primary entity is briefing-blocked: the pack is
    /// refused (no identity, no surrounding structure) rather than built
    /// (clarion-307668e2be).
    briefing_blocked: Option<String>,
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
            "tool": "entity_source_get",
            "args": {"id": primary_id},
            "why": "read the entity's source with line numbers",
        }),
        json!({
            "tool": "entity_summary_preview_cost_get",
            "args": {"id": primary_id},
            "why": "estimate the cost of an LLM briefing before spending",
        }),
    ];
    // A subsystem's useful drill-down is its members; for any other kind it is
    // the owning subsystem.
    if primary_kind == Some("subsystem") {
        reads.push(json!({
            "tool": "subsystem_member_list",
            "args": {"id": primary_id},
            "why": "list the entities clustered into this subsystem",
        }));
    } else {
        reads.push(json!({
            "tool": "entity_subsystem_get",
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
            "tool": "entity_orientation_pack_get",
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
pub(crate) fn parse_to_unix_seconds(value: &str) -> Option<i64> {
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

/// One static call Loomweave could not bind (kept separate from resolved sites).
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

    // Resolve each site's owning file + disclosure state once, mapping the byte
    // anchor to a line only after scanner and drift guards pass.
    let mut owner_meta: HashMap<String, OwnerMeta> = HashMap::new();
    let mut file_content: HashMap<String, Option<Vec<u8>>> = HashMap::new();
    // The queried entity is known without a lookup.
    owner_meta.insert(entity.id.clone(), OwnerMeta::from_entity(entity.clone()));

    let mut site_values = Vec::new();
    let mut truncated = false;
    for site in resolved {
        if site_values.len() >= CALL_SITES_MAX {
            truncated = true;
            break;
        }
        let owner = resolve_owner(conn, &mut owner_meta, &site.owner_id)?;
        if !path.admits(owner.path.as_deref()) {
            continue;
        }
        let anchor = anchor_line(&mut file_content, owner, site.byte_start);
        site_values.push(json!({
            "edge_kind": site.edge_kind,
            "other_id": site.other_id,
            "confidence": site.confidence.as_str(),
            "file": owner.path,
            "line": anchor.line,
            "column": anchor.column,
            "line_text": anchor.line_text,
            "source_status": anchor.source_status,
            "briefing_blocked": anchor.briefing_blocked,
            "drift": anchor.drift,
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
        let owner = resolve_owner(conn, &mut owner_meta, &site.owner_id)?;
        if !path.admits(owner.path.as_deref()) {
            continue;
        }
        let anchor = anchor_line(&mut file_content, owner, Some(site.byte_start));
        unresolved_values.push(json!({
            "callee_expr": site.callee_expr,
            "file": owner.path,
            "line": anchor.line,
            "column": anchor.column,
            "line_text": anchor.line_text,
            "source_status": anchor.source_status,
            "briefing_blocked": anchor.briefing_blocked,
            "drift": anchor.drift,
            "byte_start": site.byte_start,
            "byte_end": site.byte_end
        }));
    }

    Ok(Some(json!({
        "entity": entity_json(conn, &entity),
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

#[derive(Clone)]
struct OwnerMeta {
    entity: Option<EntityRow>,
    path: Option<String>,
    briefing_blocked: bool,
}

impl OwnerMeta {
    fn from_entity(entity: EntityRow) -> Self {
        let briefing_blocked = briefing_block_reason(&entity).is_some();
        let path = entity.source_file_path.clone();
        Self {
            entity: Some(entity),
            path,
            briefing_blocked,
        }
    }

    fn missing() -> Self {
        Self {
            entity: None,
            path: None,
            briefing_blocked: false,
        }
    }
}

struct AnchorLine {
    line: Value,
    column: Value,
    line_text: String,
    source_status: &'static str,
    briefing_blocked: bool,
    drift: Value,
}

impl AnchorLine {
    fn redacted(source_status: &'static str, briefing_blocked: bool, drift: Value) -> Self {
        Self {
            line: Value::Null,
            column: Value::Null,
            line_text: String::new(),
            source_status,
            briefing_blocked,
            drift,
        }
    }

    fn ok(line: i64, column: i64, line_text: String) -> Self {
        Self {
            line: json!(line),
            column: json!(column),
            line_text,
            source_status: "ok",
            briefing_blocked: false,
            drift: Value::Null,
        }
    }
}

/// Memoized lookup of an owner entity's disclosure metadata. A
/// briefing-blocked owner's source bytes must never be read — the pre-ingest
/// scanner withholds them — so `call_sites` redacts `line_text` for such owners
/// rather than disclosing the file content behind an edge.
fn resolve_owner<'a>(
    conn: &rusqlite::Connection,
    cache: &'a mut HashMap<String, OwnerMeta>,
    owner_id: &str,
) -> Result<&'a OwnerMeta, StorageError> {
    if !cache.contains_key(owner_id) {
        let meta = match entity_by_id(conn, owner_id)? {
            Some(entity) => OwnerMeta::from_entity(entity),
            None => OwnerMeta::missing(),
        };
        cache.insert(owner_id.to_owned(), meta);
    }
    Ok(cache
        .get(owner_id)
        .expect("owner metadata inserted before lookup"))
}

/// Map a byte anchor to line evidence after enforcing source disclosure guards.
/// Any piece that can't be resolved degrades to JSON null / empty rather than
/// failing the whole query.
fn anchor_line(
    file_content: &mut HashMap<String, Option<Vec<u8>>>,
    owner: &OwnerMeta,
    byte_start: Option<i64>,
) -> AnchorLine {
    if owner.briefing_blocked {
        return AnchorLine::redacted("briefing_blocked", true, Value::Null);
    }
    let (Some(entity), Some(path), Some(byte_start)) =
        (owner.entity.as_ref(), owner.path.as_deref(), byte_start)
    else {
        return AnchorLine::redacted("unavailable", false, Value::Null);
    };

    let bytes = file_content
        .entry(path.to_owned())
        .or_insert_with(|| std::fs::read(path).ok());
    let Some(bytes) = bytes.as_deref() else {
        return AnchorLine::redacted("missing", false, Value::Null);
    };
    let Ok(content) = String::from_utf8(bytes.to_vec()) else {
        return AnchorLine::redacted("binary", false, Value::Null);
    };

    if let (Some(stored), Some(current)) = (
        entity.content_hash.as_deref(),
        current_source_content_hash(entity, bytes, Some(&content)),
    ) && stored != current
    {
        return AnchorLine::redacted(
            "drifted",
            false,
            json!({
                "stored_content_hash": stored,
                "current_content_hash": current
            }),
        );
    }

    match byte_line_col(&content, byte_start) {
        Some((line, column)) => AnchorLine::ok(line, column, line_text_at(&content, line)),
        None => AnchorLine::redacted("unavailable", false, Value::Null),
    }
}

/// Build the `source_for_entity` payload: the entity's exact indexed line span
/// plus `context_lines` of surrounding context, line-numbered and drift-checked.
///
/// Returns an explicit `source_status` rather than a stale or misleading
/// snippet when the source cannot be trusted: `missing` (file gone),
/// `no_source_path` / `no_range` (no anchor to read), `binary` (non-UTF-8), or
/// `drifted` (the file no longer hashes to the indexed `content_hash`).
fn source_for_entity_json(
    conn: &rusqlite::Connection,
    entity: &EntityRow,
    context_lines: usize,
) -> Value {
    let identity = entity_json(conn, entity);

    // Refuse to read or return bytes for an entity whose file the pre-ingest
    // scanner marked `briefing_blocked`. Without this guard, an agent holding
    // the id of a function/class in a secret-bearing file could use
    // source_for_entity to disclose exactly the bytes the scanner policy
    // withholds (the summary / HTTP read surfaces already refuse these).
    if let Some(reason) = briefing_block_reason(entity) {
        return json!({
            "entity": identity,
            "source_status": "briefing_blocked",
            "briefing_blocked": reason
        });
    }

    let Some(path) = entity.source_file_path.as_deref() else {
        return json!({"entity": identity, "source_status": "no_source_path"});
    };

    let source_anchor = source_anchor_for_entity(conn, entity, path);
    if let Some(reason) = source_anchor
        .as_ref()
        .and_then(|anchor| anchor.briefing_blocked.as_deref())
    {
        return json!({
            "entity": identity,
            "source_file_path": path,
            "source_status": "briefing_blocked",
            "briefing_blocked": reason
        });
    }

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

    let (emitted_context_lines, context_omitted_reason) =
        match guard_source_context(entity, context_lines, source_anchor.as_ref(), &bytes) {
            SourceContextGuard::Emit(lines) => (lines, None),
            SourceContextGuard::Omit(reason) => (0, Some(reason)),
            SourceContextGuard::Drift {
                stored_content_hash,
                current_content_hash,
            } => {
                return json!({
                    "entity": identity,
                    "source_file_path": path,
                    "source_status": "drifted",
                    "drift": {
                        "stored_content_hash": stored_content_hash,
                        "current_content_hash": current_content_hash
                    }
                });
            }
        };

    let lines: Vec<&str> = source.lines().collect();
    let total = i64::try_from(lines.len()).unwrap_or(i64::MAX);
    // Clamp the span to the file, then widen by the context window. 1-based,
    // inclusive on both ends.
    let span_start = start_line.max(1);
    let span_end = end_line.min(total).max(span_start);
    let ctx = i64::try_from(emitted_context_lines).unwrap_or(i64::MAX);
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
        "context_lines": emitted_context_lines,
        "requested_context_lines": context_lines,
        "context_omitted_reason": context_omitted_reason,
        "window_start": window_start,
        "window_end": window_end,
        "lines": emitted,
        "truncated": truncated
    })
}

enum SourceContextGuard {
    Emit(usize),
    Omit(&'static str),
    Drift {
        stored_content_hash: String,
        current_content_hash: String,
    },
}

fn guard_source_context(
    entity: &EntityRow,
    context_lines: usize,
    source_anchor: Option<&SourceAnchorGuard>,
    bytes: &[u8],
) -> SourceContextGuard {
    // Non-file-scope entities store a span hash, but context lines are outside
    // that span. Only return caller-requested context when we can verify the
    // enclosing source-file hash; otherwise emit the exact entity span only.
    if context_lines == 0 || is_plugin_file_scope_entity(entity) {
        return SourceContextGuard::Emit(context_lines);
    }
    let Some(stored_file_hash) = source_anchor.and_then(|anchor| anchor.content_hash.as_deref())
    else {
        return SourceContextGuard::Omit("unverified_context");
    };
    let current_file_hash = blake3::hash(bytes).to_hex().to_string();
    if stored_file_hash == current_file_hash {
        SourceContextGuard::Emit(context_lines)
    } else {
        SourceContextGuard::Drift {
            stored_content_hash: stored_file_hash.to_owned(),
            current_content_hash: current_file_hash,
        }
    }
}

#[derive(Debug, Clone)]
struct SourceAnchorGuard {
    content_hash: Option<String>,
    briefing_blocked: Option<String>,
}

fn source_anchor_for_entity(
    conn: &rusqlite::Connection,
    entity: &EntityRow,
    source_file_path: &str,
) -> Option<SourceAnchorGuard> {
    if let Some(source_file_id) = entity.source_file_id.as_deref()
        && source_file_id != entity.id
        && let Ok(Some(anchor)) = entity_by_id(conn, source_file_id)
    {
        let briefing_blocked = briefing_block_reason(&anchor);
        return Some(SourceAnchorGuard {
            content_hash: anchor.content_hash,
            briefing_blocked,
        });
    }

    conn.query_row(
        "SELECT properties, content_hash \
         FROM entities \
         WHERE source_file_path = ?1 AND kind = 'file' \
         ORDER BY CASE plugin_id WHEN 'core' THEN 0 ELSE 1 END, id ASC \
         LIMIT 1",
        [source_file_path],
        |row| {
            let properties_json: String = row.get(0)?;
            let content_hash: Option<String> = row.get(1)?;
            Ok(SourceAnchorGuard {
                content_hash,
                briefing_blocked: entity_briefing_block_reason(&properties_json),
            })
        },
    )
    .optional()
    .ok()
    .flatten()
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
    if is_plugin_file_scope_entity(entity) {
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
    // Candidate entities for the LLM inference prompt — an internal payload,
    // not a tool return surface, so it carries identity fields only (no `sei`).
    serde_json::to_string(
        &entities
            .iter()
            .map(entity_identity_json)
            .collect::<Vec<_>>(),
    )
    .expect("candidate entity JSON serializes")
}

fn inferred_records_from_result(
    read: &InferredRead,
    result_json: &str,
    max_edges: usize,
) -> Result<Vec<InferredCallEdgeRecord>, InferredDispatchFailure> {
    let parsed: InferredCallsResponse = serde_json::from_str(result_json).map_err(|err| {
        InferredDispatchFailure::new(
            McpErrorCode::LlmInvalidJson,
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

fn guidance_authored_at_from_clock(raw: &str) -> String {
    const ISO_MILLIS_UTC: &[time::format_description::FormatItem<'_>] =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

    let parsed = if let Some(seconds) = raw.strip_prefix("unix:") {
        seconds
            .trim()
            .parse::<i64>()
            .ok()
            .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
    } else {
        parse_to_unix_seconds(raw)
            .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
    };

    parsed
        .and_then(|timestamp| timestamp.format(&ISO_MILLIS_UTC).ok())
        .unwrap_or_else(|| raw.to_owned())
}

fn caller_json(
    conn: &rusqlite::Connection,
    edge: &CallEdgeMatch,
) -> Result<Option<Value>, StorageError> {
    Ok(entity_by_id(conn, &edge.from_id)?.map(|entity| {
        json!({
            "entity": entity_json(conn, &entity),
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
        // `stored_to_id` echoes the callee's raw id, which leaks the qualname of
        // a blocked callee even when `entity_json` redacts it (clarion-307668e2be).
        let stored_to_id = if briefing_block_reason(&entity).is_some() {
            Value::Null
        } else {
            json!(edge.stored_to_id)
        };
        json!({
            "entity": entity_json(conn, &entity),
            "edge_confidence": edge.confidence.as_str(),
            "source_byte_start": edge.source_byte_start,
            "source_byte_end": edge.source_byte_end,
            "stored_to_id": stored_to_id
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
    // A briefing-blocked node is omitted from the node table (its id IS its
    // qualname, so it cannot be projected) and its occurrences in the path
    // arrays are replaced with a sentinel. The path keeps its shape — a flow
    // *through* withheld territory — without disclosing which entity
    // (clarion-307668e2be).
    let mut blocked: BTreeSet<String> = BTreeSet::new();
    let mut nodes = Vec::new();
    for id in &node_ids {
        if let Some(entity) = entity_by_id(conn, id)? {
            if briefing_block_reason(&entity).is_some() {
                blocked.insert(id.clone());
            } else {
                nodes.push(compact_node_json(conn, &entity));
            }
        }
    }
    if !blocked.is_empty() {
        for path in &mut paths {
            for id in path.iter_mut() {
                if blocked.contains(id) {
                    BRIEFING_BLOCKED_PATH_SENTINEL.clone_into(id);
                }
            }
        }
    }
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
fn compact_node_json(conn: &rusqlite::Connection, entity: &EntityRow) -> Value {
    // REQ-C-04: a path node is the only representation of that entity in the
    // response, so it carries its `sei` (the durable binding key) alongside the
    // mutable `id`/locator. Graceful-degrade to null on a pre-SEI DB.
    json!({
        "id": entity.id,
        "sei": sei_for_locator(conn, &entity.id).ok().flatten(),
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
/// file-scope altitude when the entity is a manifest-declared file-scope entity
/// (clarion-79d0ff6e14).
///
/// References are tracked symbol-to-symbol, so a file-scope entity's OWN
/// reference edges are almost always empty — "who imports this module /
/// contract?" used to answer `[]`. For a file-scope entity we instead aggregate
/// the `references` edges of every transitively contained symbol (excluding
/// intra-file wiring) and tag each neighbor with the contained `via` symbol it
/// touches. For any other kind the direct symbol-level edges are returned
/// unchanged (no `via`).
///
/// Returns `(neighbors, rolled_up)`; `rolled_up` is true only for file scopes.
fn reference_neighbors_for(
    conn: &rusqlite::Connection,
    entity: &EntityRow,
    direction: ReferenceDirection,
) -> Result<(Vec<Value>, bool), StorageError> {
    if is_plugin_file_scope_entity(entity) {
        let edges = module_reference_rollup(conn, &entity.id, direction)?;
        Ok((rolled_up_neighbors_json(conn, edges, direction)?, true))
    } else {
        Ok((reference_neighbors(conn, &entity.id, direction)?, false))
    }
}

fn is_plugin_file_scope_entity(entity: &EntityRow) -> bool {
    if entity.plugin_id == "core" {
        return false;
    }
    if let Some(source_file_id) = entity.source_file_id.as_deref()
        && entity.parent_id.as_deref() == Some(source_file_id)
        && source_file_id.starts_with("core:file:")
    {
        return true;
    }
    entity.kind == "module"
        && entity.parent_id.is_none()
        && entity.source_file_path.is_some()
        && entity.source_line_start.is_some()
        && entity.source_line_end.is_some()
}

fn rolled_up_neighbors_json(
    conn: &rusqlite::Connection,
    edges: Vec<RolledUpReferenceEdge>,
    direction: ReferenceDirection,
) -> Result<Vec<Value>, StorageError> {
    let mut neighbors = Vec::new();
    for edge in edges {
        if let Some(entity) = entity_by_id(conn, &edge.neighbor_id)? {
            let via = entity_by_id(conn, &edge.via_id)?;
            let mut object = serde_json::Map::new();
            object.insert("entity".to_owned(), entity_json(conn, &entity));
            object.insert(
                "edge_confidence".to_owned(),
                json!(edge.confidence.as_str()),
            );
            object.insert(
                "source_byte_start".to_owned(),
                json!(edge.source_byte_start),
            );
            object.insert("source_byte_end".to_owned(), json!(edge.source_byte_end));
            // The module-contained symbol this edge actually touches, so a
            // rolled-up "who imports this module" answer names the importer
            // (entity) AND the imported symbol (via).
            object.insert(
                "via".to_owned(),
                via.as_ref()
                    .map_or(Value::Null, |entity| entity_json(conn, entity)),
            );
            // Reverse-import altitude (clarion-79d0ff6e14): the edge is recorded
            // against the importing *symbol*, but "who imports this module /
            // contract" is a module-altitude question, so name the importer's
            // containing module too. Only meaningful for the `In` direction
            // (the neighbor is the importer); `Out` neighbors are referenced
            // symbols, not importers. `null` for an importer with no module
            // ancestor.
            if direction == ReferenceDirection::In {
                let importer_module = match containing_module_id(conn, &edge.neighbor_id)? {
                    Some(module_id) => entity_by_id(conn, &module_id)?,
                    None => None,
                };
                object.insert(
                    "importer_module".to_owned(),
                    importer_module
                        .as_ref()
                        .map_or(Value::Null, |entity| entity_json(conn, entity)),
                );
            }
            neighbors.push(Value::Object(object));
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
                "entity": entity_json(conn, &entity),
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
    use std::sync::Arc;
    use std::time::Duration;

    use loomweave_core::{CachingModel, LlmProvider, LlmProviderError, LlmRequest, LlmResponse};
    use loomweave_storage::{
        EntityRow, InferredEdgeCacheKey, ReaderPool, UnresolvedCallSiteRow, pragma, schema,
    };
    use rusqlite::Connection;
    use tokio::sync::mpsc;

    use super::{
        InferenceLlmState, InferredRead, McpToolPolicy, ServerState, config::LlmConfig, list_tools,
    };

    #[test]
    fn tools_list_exposes_exact_docstrings() {
        let tools = list_tools();

        assert_eq!(tools.len(), 40);
        assert_eq!(tools[0].name, "entity_at");
        assert_eq!(
            tools[0].description,
            "Return the innermost Loomweave entity whose source range contains a file and line, plus an `entity_context` evidence block: match_reason (decorator_range / declaration / body_range / containing_range / no_match) explaining why the line matched, the module→entity containing stack, the matched entity's decl/body/decorator sub-ranges, any same-granularity ambiguity alternatives, and index freshness. Paths are normalized relative to the project root. A blank or comment line that only a module spans reports containing_range — never a fabricated exact match."
        );
        assert_eq!(tools[1].name, "entity_find");
        assert_eq!(
            tools[1].description,
            "Search Loomweave entities by id, name, short name, summary, and docstring content. Matching merges stemmed FTS ranking with grep-equivalent substring recall, so a concept word finds both entities whose docstring mentions it and identifiers that merely contain it (e.g. `library` finds the class `LibraryService`, which whole-token FTS alone misses). This is the always-on keyword-discovery path — no embeddings required (semantic ranking is the separate, opt-in `entity_semantic_search_list`). Results are paginated; FTS-ranked hits come first, then substring-only hits. Docstrings withheld by the secret scanner (briefing_blocked) are never matched. This does not traverse the graph and does not search on-demand summary_cache entries. Pass an optional `kind` (e.g. \"subsystem\", \"function\", \"class\", \"module\") to return only entities of that kind — the way to locate a subsystem without visually filtering results."
        );
        assert_eq!(tools[2].name, "entity_callers_list");
        assert_eq!(
            tools[2].description,
            "Return entities that call the given entity. Default confidence is resolved, so ambiguous static candidates and LLM-inferred edges are excluded unless explicitly requested. Ambiguous edges expand all candidates; inferred edges may trigger bounded LLM dispatch. The result carries scope_excludes naming static blind spots not searched (e.g. attribute-receiver-calls) so an empty callers list is never read as a guaranteed true negative."
        );
        assert_eq!(tools[3].name, "entity_execution_path_list");
        assert_eq!(
            tools[3].description,
            "Return bounded calls-only execution paths starting at an entity. Default confidence is resolved. max_depth defaults to 3. Results are compact: a deduplicated nodes table plus paths as arrays of node ids (under a root), ranked longest-first. Traversal stops at the server edge cap and the response is capped at a maximum number of ranked paths; truncated/truncation_reason report edge-cap or path-cap when either trims. The result carries scope_excludes naming static blind spots not searched (e.g. attribute-receiver-calls)."
        );
        assert_eq!(tools[4].name, "entity_summary_get");
        assert_eq!(
            tools[4].description,
            "Return an on-demand cached summary for one entity. In v0.1 this is leaf scope only: module summaries describe the module docstring and top-level members, not an aggregation of contained function/class summaries. If the LLM returns non-JSON the response degrades to a deterministic structural summary (kind: structural-fallback) built from the entity source, and that fallback is cached so a retry is a free cache hit rather than a re-billed failure."
        );
        assert_eq!(tools[5].name, "entity_issue_list");
        assert_eq!(
            tools[5].description,
            "Return Filigree issues attached to this Loomweave entity, optionally including issues attached to contained entities. Filigree is an enrichment source; if unavailable, the tool returns an unavailable envelope instead of failing Loomweave. The result carries a result_kind (matched | no_matches | unavailable) so a reachable-but-empty Filigree is distinct from an unreachable one, and a filigree_endpoint block (configured vs resolved URL + resolution_source) so you can see which endpoint — e.g. a live ethereal port — the answer came from. Each matched/drifted entry carries an `issue` object with the issue's title, status, and priority (fetched once per distinct issue, no N+1); `issue` is null when the issue-detail route is unavailable, so the match still resolves without a second hop into Filigree. Includes a `wardline_findings` section (enrich-only) reconciling Wardline findings to the entity by qualname; `result_kind` is matched|no_matches|unavailable."
        );
        assert_eq!(tools[6].name, "entity_neighborhood_get");
        assert_eq!(
            tools[6].description,
            "Return the one-hop Loomweave neighborhood around an entity: callers, callees, container, contained entities, references, and imports (imports_in = who imports this module, imports_out = what it imports; module-to-module). Default confidence is resolved; ambiguous and inferred calls are opt-in. References and imports are not execution flow. When the entity is a module, references_in/references_out are rolled up over the symbols it contains (references_rolled_up=true) — each neighbor carries a `via` naming the contained symbol the edge touches, so \"who imports this module/contract\" is answered at module altitude rather than reading empty. On references_in each rolled-up neighbor also carries `importer_module` — the importing symbol's containing module — so reverse-import names importing modules, not just symbols. The result carries scope_excludes naming blind spots not searched (e.g. attribute-receiver-calls) so empty sections are never read as guaranteed true negatives."
        );
        assert_eq!(tools[7].name, "subsystem_member_list");
        assert_eq!(
            tools[7].description,
            "List module entities assigned to a subsystem entity."
        );
        assert_eq!(tools[8].name, "entity_subsystem_get");
        assert_eq!(
            tools[8].description,
            "Return the subsystem an entity belongs to — the reverse of subsystem_members. Accepts any entity id: a module resolves directly, while a function/class resolves through its nearest containing module. Returns the subsystem id/name and the module the membership was resolved through, or a no-subsystem result when the entity has no subsystem-assigned module ancestor."
        );
        assert_eq!(tools[9].name, "project_status_get");
        assert_eq!(
            tools[9].description,
            "Return deterministic Loomweave diagnostics: repo root, db path, latest run (id/status/started/completed), entity/subsystem/edge/finding/briefing-blocked counts, index staleness, per-plugin entity counts from the current index, LLM policy (provider/live/cache), and the resolved Filigree endpoint (configured vs resolved URL + resolution source). Answers \"is the graph fresh, plugin-less, LLM-live, Filigree-reachable?\" without shelling out. No LLM call."
        );
        assert_eq!(tools[10].name, "entity_summary_preview_cost_get");
        assert_eq!(
            tools[10].description,
            "Preview what calling summary(id) would cost BEFORE spending. Reports cache_status (hit | expired | miss), the cached row's real tokens/cost/age on a hit, an input-token estimate on a miss, the configured model, the LLM policy (provider/live/allow_live_provider/cache horizon), and live_spend_would_occur — true only when no fresh cache row exists AND a live provider is wired. A disabled/unconfigured LLM is reported distinctly from a cache miss. Never invokes the LLM provider."
        );
        assert_eq!(tools[11].name, "entity_source_get");
        assert_eq!(
            tools[11].description,
            "Return the exact indexed source span for one entity (its source_line_start..source_line_end, which includes any decorators/signature/docstring the plugin captured) plus a bounded window of surrounding context, as line-numbered lines each flagged in_entity true/false. No LLM call. Lets an agent read and trust the entity without shelling out. source_status reports `ok`, or — instead of a misleading stale snippet — `missing` (file gone), `no_range`/`no_source_path` (entity has no anchor), `binary` (non-UTF-8), or `drifted` (the file no longer matches the indexed content_hash; rerun `loomweave analyze`). context_lines defaults to 10."
        );
        assert_eq!(tools[12].name, "entity_call_site_list");
        assert_eq!(
            tools[12].description,
            "Show the actual source sites behind calls/references edges, so an agent can see WHY Loomweave believes an edge exists rather than trusting it blind. role=caller (default) returns this entity's outgoing sites (what it calls/references); role=callee returns incoming sites (who calls/references it). Each site carries the file path, 1-based line, byte column, the source line text, edge kind, confidence, and a resolution of resolved | ambiguous (with candidate ids) | unresolved (a static call Loomweave could not bind, kept separate so it is never mixed with resolved evidence). Filter by edge kind (`calls`/`references`) and by a best-effort production/test path heuristic (`all`/`production`/`test`; path partitioning is not indexed — the heuristic matches conventional test paths). Output is bounded; truncated flags when the site cap trims. No LLM call."
        );
        assert_eq!(tools[13].name, "entity_orientation_pack_get");
        assert_eq!(
            tools[13].description,
            "Assemble one deterministic orientation packet for a code location — the replacement for hand-composing find_entity + entity_at + source reads + neighborhood + issues_for + freshness on every question. Resolve EITHER by `entity` id OR by `file`+`line` (exactly one form). The packet bundles: the primary entity, the entity_context evidence (match_reason / containing stack / decl-body-decorator ranges — so a decorator-line query is explained, not guessed), a compact source-span summary, one-hop neighbors (callers, callees, container, contained, references, imports — for a module, references_in/out are rolled up over contained symbols with references_rolled_up=true), compact resolved execution paths, related Filigree issues, index/Filigree/LLM health, warnings, and suggested next reads. No LLM summary is invoked. Every list is bounded; an `omitted` block reports per-section truncation counts and `degraded` sections name surfaces that were unavailable (e.g. Filigree down) so an empty section is never read as a guaranteed negative. Includes a `wardline_findings` section (enrich-only) reconciling Wardline findings to the entity by qualname; `result_kind` is matched|no_matches|unavailable."
        );
        assert_eq!(tools[14].name, "analyze_start");
        assert_eq!(
            tools[14].description,
            "Start a `loomweave analyze` run over this project in the background and return its run handle immediately — do not block on the (possibly many-minute) run. Re-indexes the source tree and refreshes entities/edges/subsystems. Returns run_id, status (`started`), and the progress-file path. Only one analyze may run per project at a time (a cross-process lock enforces it); a second start while one is active is rejected. Poll analyze_status for progress; analyze_cancel to stop. No arguments."
        );
        assert_eq!(tools[15].name, "analyze_status_get");
        assert_eq!(
            tools[15].description,
            "Report the live status of an analyze run started via analyze_start. status is one of queued (spawned, not yet recording) | running | completed | failed | cancelled | skipped_no_plugins. While running it exposes phase (discovering / analyzing / clustering), current_plugin, processed_files / total_files, current_file, the latest heartbeat_at, elapsed_seconds, and progress_observed (false when the heartbeat has gone stale — the run may be wedged). On a terminal status it carries the recorded run stats. Reads structured progress, never logs."
        );
        assert_eq!(tools[16].name, "analyze_cancel");
        assert_eq!(
            tools[16].description,
            "Cancel a running analyze. SIGKILLs the run's whole process group — terminating the language plugin and its pyright-langserver child — then marks the run terminal (status `cancelled`) so it is never left dangling as `running`. Idempotent: cancelling an already-terminal run reports its current state. Partial work already written is kept (cancel discards in-flight work, not the index)."
        );
        assert_eq!(tools[17].name, "index_diff_get");
        assert_eq!(tools[18].name, "entity_guidance_list");
        assert_eq!(tools[19].name, "propose_guidance");
        assert_eq!(tools[20].name, "promote_guidance");
        assert_eq!(tools[21].name, "entity_finding_list");
        assert_eq!(tools[22].name, "entity_wardline_get");
        assert_eq!(tools[23].name, "entity_tag_list");
        assert_eq!(tools[24].name, "entity_kind_list");
        assert_eq!(tools[25].name, "entity_wardline_list");
        assert_eq!(tools[26].name, "module_circular_import_list");
        assert_eq!(tools[27].name, "entity_coupling_hotspot_list");
        assert_eq!(tools[28].name, "entity_entry_point_list");
        assert_eq!(tools[29].name, "entity_http_route_list");
        assert_eq!(tools[30].name, "entity_data_model_list");
        assert_eq!(tools[31].name, "entity_test_list");
        assert_eq!(tools[32].name, "entity_deprecation_list");
        assert_eq!(tools[33].name, "entity_todo_list");
        assert_eq!(tools[34].name, "entity_test_caller_list");
        assert_eq!(tools[35].name, "entity_high_churn_list");
        assert_eq!(tools[36].name, "entity_recent_change_list");
        assert_eq!(tools[37].name, "entity_dead_list");
        assert_eq!(tools[38].name, "entity_semantic_search_list");
        assert_eq!(tools[39].name, "project_finding_list");
    }

    #[test]
    fn server_instructions_enumerate_every_tool() {
        // Single-source guard (clarion-71f0d6c3dd): the `instructions` tool list
        // is derived from list_tools_for_policy under the active policy, so every
        // tool the server actually registers must appear in it — and a write-gated
        // tool must NOT appear when the gate is off (agent-first-feedback §2.5).
        use super::McpToolPolicy;

        // With write tools enabled, every tool is advertised.
        let all = super::server_instructions(McpToolPolicy::allow_write_tools());
        for tool in super::list_tools_for_policy(McpToolPolicy::allow_write_tools()) {
            assert!(
                all.contains(tool.name),
                "instructions omit registered tool {:?}; instructions were:\n{all}",
                tool.name
            );
        }

        // Under the default read-only policy, the advertised list matches the
        // registered list exactly — gated write tools are absent from the list
        // but named in the gate note.
        let read_only = super::server_instructions(McpToolPolicy::default());
        let registered = super::list_tools_for_policy(McpToolPolicy::default());
        for tool in &registered {
            assert!(
                read_only.contains(tool.name),
                "instructions omit registered tool {:?}; instructions were:\n{read_only}",
                tool.name
            );
        }
        assert!(
            registered.len() < super::list_tools().len(),
            "default policy should gate at least one write tool"
        );
        // The gate note names the write tools and how to enable them.
        assert!(read_only.contains("enable_write_tools"), "{read_only}");
        assert!(read_only.contains("entity_summary_get"), "{read_only}");
    }

    #[test]
    fn no_index_initialize_chirps_install_and_analyze() {
        let root = std::path::Path::new("/tmp/demo");
        let request = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let response =
            super::handle_json_rpc_no_index(&request, root).expect("initialize yields a response");
        assert_eq!(
            response["result"]["protocolVersion"],
            super::MCP_PROTOCOL_VERSION
        );
        assert_eq!(response["result"]["serverInfo"]["name"], "loomweave");
        assert!(response["result"]["capabilities"]["tools"].is_object());
        let instructions = response["result"]["instructions"]
            .as_str()
            .expect("instructions present");
        // Both halves of the canonical hook sequence, plus the project path.
        assert!(
            instructions.contains("loomweave install --path /tmp/demo"),
            "instructions: {instructions}"
        );
        assert!(
            instructions.contains("loomweave analyze /tmp/demo"),
            "instructions: {instructions}"
        );
    }

    #[test]
    fn no_index_tools_call_returns_actionable_is_error() {
        let root = std::path::Path::new("/tmp/demo");
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "entity_find", "arguments": {"query": "foo"}}
        });
        let response = super::handle_json_rpc_no_index(&request, root).expect("response");
        // isError is the load-bearing chirp channel — fires the moment the agent
        // touches any tool, regardless of whether the client surfaced instructions.
        assert_eq!(response["result"]["isError"], serde_json::json!(true));
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .expect("tool result text");
        assert!(
            text.contains("loomweave analyze /tmp/demo"),
            "tool chirp text: {text}"
        );
    }

    #[test]
    fn no_index_tools_list_still_advertises_tools() {
        let root = std::path::Path::new("/tmp/demo");
        let request = serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"});
        let response = super::handle_json_rpc_no_index(&request, root).expect("response");
        let tools = response["result"]["tools"].as_array().expect("tools array");
        assert!(
            !tools.is_empty(),
            "degraded tools/list should still advertise the surface"
        );
    }

    #[test]
    fn no_index_ignores_notifications() {
        let root = std::path::Path::new("/tmp/demo");
        // The client sends notifications/initialized right after initialize; it
        // has no id and must draw no response.
        let request = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert!(super::handle_json_rpc_no_index(&request, root).is_none());
    }

    #[test]
    fn serve_stdio_no_index_round_trips_initialize_over_json_line() {
        let root = std::path::Path::new("/tmp/demo");
        let input = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n";
        let mut reader = std::io::BufReader::new(&input[..]);
        let mut output = Vec::new();
        super::serve_stdio_no_index(root, &mut reader, &mut output).expect("degraded serve");
        let response: serde_json::Value = serde_json::from_slice(&output).expect("framed json");
        let instructions = response["result"]["instructions"]
            .as_str()
            .expect("instructions present");
        assert!(
            instructions.contains("loomweave analyze /tmp/demo"),
            "instructions: {instructions}"
        );
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
        assert_eq!(response["result"]["serverInfo"]["name"], "loomweave");
        assert!(response["result"]["capabilities"]["tools"].is_object());
        // Orientation instructions present and mention the skill + entity model.
        let instructions = response["result"]["instructions"]
            .as_str()
            .expect("initialize result has instructions");
        assert!(
            instructions.contains("loomweave-workflow"),
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

    #[test]
    fn optional_confidence_rejects_non_string_json_types() {
        for value in [
            serde_json::Value::Null,
            serde_json::json!(1),
            serde_json::json!(["resolved"]),
            serde_json::json!({"tier": "resolved"}),
        ] {
            let mut arguments = serde_json::Map::new();
            arguments.insert("confidence".to_owned(), value);
            let err = super::optional_confidence(&arguments).expect_err("non-string must reject");
            assert_eq!(err.message, "confidence must be a string");
        }
    }

    #[test]
    fn optional_confidence_rejects_unknown_string() {
        let mut arguments = serde_json::Map::new();
        arguments.insert("confidence".to_owned(), serde_json::json!("all"));
        let err = super::optional_confidence(&arguments).expect_err("unknown string must reject");
        assert_eq!(
            err.message,
            "confidence must be one of resolved, ambiguous, inferred"
        );
    }

    #[tokio::test]
    async fn stateful_initialize_advertises_prompts_and_resources() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
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
            instructions.contains("loomweave-workflow"),
            "instructions should point at the skill"
        );
    }

    #[tokio::test]
    async fn resources_list_includes_loomweave_context() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
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
            resources.iter().any(|r| r["uri"] == "loomweave://context"),
            "loomweave://context not listed: {resources:?}"
        );
    }

    #[tokio::test]
    async fn resources_read_returns_context_snapshot_json() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
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
                "params": {"uri": "loomweave://context"}
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
        let db = dir.path().join("loomweave.db");
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
                "params": {"uri": "loomweave://nope"}
            }))
            .await
            .expect("response");
        assert!(response["error"].is_object(), "expected an error envelope");
        assert_eq!(response["error"]["code"], -32602, "{response:?}");
    }

    #[tokio::test]
    async fn prompts_get_rejects_unknown_name() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
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
        let db = dir.path().join("loomweave.db");
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
                "params": {"name": "loomweave-workflow"}
            }))
            .await
            .expect("response");
        let text = response["result"]["messages"][0]["content"]["text"]
            .as_str()
            .unwrap();
        assert!(
            text.contains("name: loomweave-workflow"),
            "not the skill text"
        );
    }

    #[tokio::test]
    async fn prompts_list_includes_loomweave_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
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
        assert!(prompts.iter().any(|p| p["name"] == "loomweave-workflow"));
    }

    #[test]
    fn tools_list_request_wraps_read_only_tools_by_default() {
        let response = super::handle_json_rpc(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": "tools-1",
            "method": "tools/list",
            "params": {}
        }))
        .expect("tools/list request returns a response");

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], "tools-1");
        let tools = response["result"]["tools"].as_array().unwrap();
        let tool_names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
            .collect();
        assert!(tool_names.contains(&"entity_at"));
        assert!(!tool_names.contains(&"analyze_start"));
        assert!(!tool_names.contains(&"propose_guidance"));
        assert!(!tool_names.contains(&"promote_guidance"));
        assert_eq!(response["result"]["tools"][0]["name"], "entity_at");
        assert_eq!(response["result"]["tools"][0]["read_only"], true);
        assert_eq!(response["result"]["tools"][0]["writes_local_state"], false);
        assert!(
            tool_names.contains(&"subsystem_member_list"),
            "read-only list should include subsystem_member_list: {tool_names:?}"
        );
    }

    #[tokio::test]
    async fn server_policy_hides_and_blocks_write_tools_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers);

        let listed = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": "tools-readonly",
                "method": "tools/list",
                "params": {}
            }))
            .await
            .expect("tools/list response");
        let names: Vec<&str> = listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect();
        assert!(names.contains(&"entity_at"));
        assert!(!names.contains(&"analyze_start"));
        assert!(!names.contains(&"entity_summary_get"));

        let blocked = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": "blocked",
                "method": "tools/call",
                "params": {"name": "analyze_start", "arguments": {}}
            }))
            .await
            .expect("tools/call response");
        assert_eq!(blocked["error"]["code"], -32601);
        assert!(
            blocked["error"]["message"]
                .as_str()
                .unwrap()
                .contains("tool disabled by MCP tool policy")
        );
    }

    #[tokio::test]
    async fn server_policy_can_advertise_write_tools() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers)
            .with_tool_policy(McpToolPolicy::allow_write_tools());

        let listed = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": "tools-write",
                "method": "tools/list",
                "params": {}
            }))
            .await
            .expect("tools/list response");
        let tools = listed["result"]["tools"].as_array().unwrap();
        let analyze = tools
            .iter()
            .find(|tool| tool["name"] == "analyze_start")
            .expect("analyze_start advertised when write tools enabled");
        assert_eq!(analyze["read_only"], false);
        assert_eq!(analyze["writes_local_state"], true);
        assert_eq!(analyze["spawns_process"], true);
        assert!(tools.iter().any(|tool| tool["name"] == "propose_guidance"));
    }

    #[tokio::test]
    async fn stateful_tools_list_filters_write_tools_when_policy_disables_them() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers)
            .with_tool_policy(McpToolPolicy::read_only());

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": "policy-list",
                "method": "tools/list",
                "params": {}
            }))
            .await
            .expect("response");
        let tools = response["result"]["tools"].as_array().unwrap();
        let tool_names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
            .collect();
        assert!(tool_names.contains(&"project_status_get"));
        assert!(tool_names.contains(&"entity_callers_list"));
        assert!(!tool_names.contains(&"analyze_start"));
        assert!(!tool_names.contains(&"analyze_cancel"));
        assert!(!tool_names.contains(&"entity_summary_get"));
        assert!(!tool_names.contains(&"propose_guidance"));
        assert!(!tool_names.contains(&"promote_guidance"));
        let callers = tools
            .iter()
            .find(|tool| tool["name"] == "entity_callers_list")
            .expect("callers tool still listed");
        assert_eq!(callers["metadata"]["read_only"], true);
        assert_eq!(callers["metadata"]["may_call_llm"], true);
    }

    #[tokio::test]
    async fn stateful_tools_call_rejects_disabled_write_tool() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers)
            .with_tool_policy(McpToolPolicy::read_only());

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": "policy-call",
                "method": "tools/call",
                "params": {"name": "analyze_start", "arguments": {}}
            }))
            .await
            .expect("response");
        assert_eq!(response["error"]["code"], -32601);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("disabled by MCP tool policy"),
            "{response}"
        );
    }

    #[tokio::test]
    async fn stateful_tools_call_rejects_inferred_confidence_when_write_tools_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers)
            .with_tool_policy(McpToolPolicy::read_only());

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": "policy-inferred",
                "method": "tools/call",
                "params": {
                    "name": "entity_callers_list",
                    "arguments": {"id": "python:function:demo.f", "confidence": "inferred"}
                }
            }))
            .await
            .expect("response");
        assert_eq!(response["error"]["code"], -32602);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("confidence=inferred"),
            "{response}"
        );
    }

    #[tokio::test]
    async fn stateful_tools_call_rejects_malformed_confidence_arguments() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers)
            .with_tool_policy(McpToolPolicy::allow_write_tools());

        for (idx, (value, expected)) in [
            (serde_json::Value::Null, "confidence must be a string"),
            (serde_json::json!(1), "confidence must be a string"),
            (
                serde_json::json!(["resolved"]),
                "confidence must be a string",
            ),
            (
                serde_json::json!({"tier": "resolved"}),
                "confidence must be a string",
            ),
            (
                serde_json::json!("all"),
                "confidence must be one of resolved, ambiguous, inferred",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let response = state
                .handle_json_rpc(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": format!("bad-confidence-{idx}"),
                    "method": "tools/call",
                    "params": {
                        "name": "entity_callers_list",
                        "arguments": {
                            "id": "python:function:demo.f",
                            "confidence": value
                        }
                    }
                }))
                .await
                .expect("response");
            assert_eq!(response["error"]["code"], -32602, "{response}");
            assert_eq!(response["error"]["message"], expected, "{response}");
        }
    }

    #[tokio::test]
    async fn stateful_tools_call_rejects_unknown_arguments_from_strict_schema() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("loomweave.db");
        {
            let mut conn = rusqlite::Connection::open(&db).unwrap();
            pragma::apply_write_pragmas(&conn).unwrap();
            schema::apply_migrations(&mut conn).unwrap();
        }
        let readers = ReaderPool::open(&db, 4).unwrap();
        let state = ServerState::new(dir.path().to_path_buf(), readers)
            .with_tool_policy(McpToolPolicy::allow_write_tools());

        let response = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": "unknown-argument",
                "method": "tools/call",
                "params": {
                    "name": "entity_summary_get",
                    "arguments": {
                        "id": "python:function:demo.f",
                        "safety_override": true
                    }
                }
            }))
            .await
            .expect("response");
        assert_eq!(response["error"]["code"], -32602, "{response}");
        assert_eq!(
            response["error"]["message"],
            "unknown argument for entity_summary_get: safety_override",
            "{response}"
        );
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
        let frame = loomweave_core::plugin::Frame {
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
        let tools = decoded["result"]["tools"].as_array().unwrap();
        let tool_names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
            .collect();
        assert!(tool_names.contains(&"entity_at"));
        assert!(!tool_names.contains(&"propose_guidance"));
        assert!(!tool_names.contains(&"promote_guidance"));
    }

    #[test]
    fn frame_dispatch_returns_none_for_json_rpc_notifications() {
        let frame = loomweave_core::plugin::Frame {
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
        loomweave_core::plugin::write_frame(
            &mut input,
            &loomweave_core::plugin::Frame {
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
        loomweave_core::plugin::write_frame(
            &mut input,
            &loomweave_core::plugin::Frame {
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
        let first = loomweave_core::plugin::read_frame(
            &mut response_reader,
            loomweave_core::plugin::ContentLengthCeiling::new(usize::MAX),
        )
        .unwrap();
        let second = loomweave_core::plugin::read_frame(
            &mut response_reader,
            loomweave_core::plugin::ContentLengthCeiling::new(usize::MAX),
        )
        .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
        let second_json: serde_json::Value = serde_json::from_slice(&second.body).unwrap();

        assert_eq!(first_json["id"], 11);
        assert_eq!(first_json["result"]["serverInfo"]["name"], "loomweave");
        assert_eq!(second_json["id"], 12);
        let tools = second_json["result"]["tools"].as_array().unwrap();
        let tool_names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(serde_json::Value::as_str))
            .collect();
        assert!(tool_names.contains(&"entity_at"));
        assert!(!tool_names.contains(&"propose_guidance"));
        assert!(!tool_names.contains(&"promote_guidance"));
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
        let db_path = project.path().join("loomweave.db");
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
        let db_path = project.path().join("loomweave.db");
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
        loomweave_core::plugin::write_frame(
            &mut input,
            &loomweave_core::plugin::Frame {
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
        loomweave_core::plugin::write_frame(
            &mut input,
            &loomweave_core::plugin::Frame {
                body: serde_json::to_vec(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized",
                    "params": {}
                }))
                .unwrap(),
            },
        )
        .unwrap();
        loomweave_core::plugin::write_frame(
            &mut input,
            &loomweave_core::plugin::Frame {
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
        let first = loomweave_core::plugin::read_frame(
            &mut response_reader,
            loomweave_core::plugin::ContentLengthCeiling::new(usize::MAX),
        )
        .unwrap();
        let second = loomweave_core::plugin::read_frame(
            &mut response_reader,
            loomweave_core::plugin::ContentLengthCeiling::new(usize::MAX),
        )
        .unwrap();
        let first_json: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
        let second_json: serde_json::Value = serde_json::from_slice(&second.body).unwrap();

        assert_eq!(first_json["id"], initialize_id);
        assert_eq!(second_json["id"], tools_list_id);
        assert!(
            loomweave_core::plugin::read_frame(
                &mut response_reader,
                loomweave_core::plugin::ContentLengthCeiling::new(usize::MAX),
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
        let db_path = project.path().join("loomweave.db");
        let mut conn = Connection::open(&db_path).expect("open sqlite");
        pragma::apply_write_pragmas(&conn).expect("write pragmas");
        schema::apply_migrations(&mut conn).expect("apply migrations");
        drop(conn);

        let readers = ReaderPool::open(&db_path, 1).expect("reader pool");
        let state = Arc::new(ServerState::new(project.path().to_path_buf(), readers));
        let key = inferred_test_key();
        let read = inferred_test_read(key.clone());
        let (writer, _rx) = mpsc::channel(1);
        let (release_tx, release_rx) = tokio::sync::mpsc::channel(1);
        let llm = InferenceLlmState {
            writer,
            config: LlmConfig::default(),
            provider: Arc::new(BlockingProvider {
                release: tokio::sync::Mutex::new(release_rx),
                started: None,
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
        let _ = release_tx.send(()).await;

        assert!(
            removed,
            "aborted inferred-dispatch leader left stale in-flight key"
        );
    }

    #[tokio::test]
    async fn cancellation_notification_aborts_in_flight_llm_request() {
        let project = tempfile::tempdir().expect("temp project");
        let source_path = project.path().join("demo.py");
        std::fs::write(&source_path, "def target():\n    return 1\n").expect("write source");
        let content_hash = blake3::hash("def target():\n    return 1".as_bytes())
            .to_hex()
            .to_string();
        let db_path = project.path().join("loomweave.db");
        let mut conn = Connection::open(&db_path).expect("open sqlite");
        pragma::apply_write_pragmas(&conn).expect("write pragmas");
        schema::apply_migrations(&mut conn).expect("apply migrations");
        conn.execute(
            "INSERT INTO entities (
                id, plugin_id, kind, name, short_name, source_file_path,
                source_line_start, source_line_end, properties, content_hash,
                created_at, updated_at
             ) VALUES (
                'python:function:demo.target', 'python', 'function', 'target', 'target', ?1,
                1, 2, '{}', ?2,
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
            rusqlite::params![source_path.display().to_string(), content_hash],
        )
        .expect("insert entity");
        drop(conn);

        let readers = ReaderPool::open(&db_path, 1).expect("reader pool");
        let (writer, _rx) = mpsc::channel(1);
        let (started_tx, mut started_rx) = tokio::sync::mpsc::channel(1);
        let (_release_tx, release_rx) = tokio::sync::mpsc::channel(1);
        let config = LlmConfig {
            enabled: true,
            allow_live_provider: true,
            ..LlmConfig::default()
        };
        let state = Arc::new(
            ServerState::new(project.path().to_path_buf(), readers)
                .with_tool_policy(McpToolPolicy::allow_write_tools())
                .with_summary_llm(
                    writer,
                    config,
                    Arc::new(BlockingProvider {
                        started: Some(started_tx),
                        release: tokio::sync::Mutex::new(release_rx),
                    }),
                ),
        );

        let request_state = Arc::clone(&state);
        let handle = tokio::spawn(async move {
            request_state
                .handle_json_rpc(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 99,
                    "method": "tools/call",
                    "params": {
                        "name": "entity_summary_get",
                        "arguments": {"id": "python:function:demo.target"}
                    }
                }))
                .await
                .expect("cancelled response")
        });
        wait_until_active_request(&state, "n:99").await;
        tokio::time::timeout(Duration::from_secs(2), started_rx.recv())
            .await
            .expect("provider should start before cancellation")
            .expect("provider start channel should remain open");

        let notification = state
            .handle_json_rpc(&serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": 99}
            }))
            .await;
        assert!(notification.is_none());

        let response = tokio::time::timeout(Duration::from_secs(2), handle)
            .await
            .expect("cancelled request should complete promptly")
            .expect("request task should not panic");
        assert_eq!(response["id"], 99);
        assert_eq!(response["error"]["code"], -32800);
        assert_eq!(response["error"]["message"], "request cancelled");
        wait_until_inactive_request(&state, "n:99").await;
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

    async fn wait_until_active_request(state: &ServerState, request_id: &str) {
        for _ in 0..50 {
            if state.active_requests.lock().await.contains(request_id) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("request {request_id} never became active");
    }

    async fn wait_until_inactive_request(state: &ServerState, request_id: &str) {
        for _ in 0..50 {
            if !state.active_requests.lock().await.contains(request_id)
                && !state.cancelled_requests.lock().await.contains(request_id)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("request {request_id} stayed active after cancellation");
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

    #[test]
    fn source_for_entity_blocks_briefing_blocked_file_without_leaking_bytes() {
        // An entity whose source file the pre-ingest scanner marked
        // briefing_blocked must never have its bytes returned by
        // source_for_entity — that path would otherwise bypass the
        // secret-redaction policy the scanner enforces.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.py");
        std::fs::write(&path, "API_KEY = 'super-secret-value'\n").unwrap();
        let mut entity = entity_row("python:function:demo.secret", "secret", None);
        entity.source_file_path = Some(path.to_string_lossy().into_owned());
        entity.source_line_start = Some(1);
        entity.source_line_end = Some(1);
        entity.properties_json =
            r#"{"briefing_blocked":"secret detected by pre-ingest scanner"}"#.to_owned();

        // A migrated connection satisfies the SEI read-time join
        // (`entity_json`); with no bindings seeded, `sei` degrades to null,
        // which is irrelevant to this briefing-block assertion. WAL (ADR-011)
        // requires a file-backed DB, so use the tempdir rather than :memory:.
        let db_path = dir.path().join("loomweave.db");
        let mut conn = Connection::open(&db_path).expect("open sqlite");
        pragma::apply_write_pragmas(&conn).expect("write pragmas");
        schema::apply_migrations(&mut conn).expect("apply migrations");

        let out = super::source_for_entity_json(&conn, &entity, 10);

        assert_eq!(out["source_status"], "briefing_blocked");
        assert_eq!(
            out["briefing_blocked"],
            "secret detected by pre-ingest scanner"
        );
        assert!(
            out.get("lines").is_none(),
            "must not return source lines: {out}"
        );
        assert!(
            !out.to_string().contains("super-secret-value"),
            "leaked briefing-blocked bytes: {out}"
        );
    }

    #[test]
    fn source_for_entity_blocks_when_only_the_file_anchor_is_briefing_blocked() {
        // The child entity itself carries no briefing_blocked flag, but its
        // enclosing source file does. source_for_entity must still refuse to
        // read or return bytes: resolving the file anchor (here via
        // `source_file_id`) and honouring its briefing_blocked flag is what
        // stops an agent from reaching secret-bearing bytes through an
        // individually-unmarked child entity.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secret.py");
        std::fs::write(&path, "API_KEY = 'super-secret-value'\n").unwrap();

        let db_path = dir.path().join("loomweave.db");
        let mut conn = Connection::open(&db_path).expect("open sqlite");
        pragma::apply_write_pragmas(&conn).expect("write pragmas");
        schema::apply_migrations(&mut conn).expect("apply migrations");

        // Seed the file anchor the pre-ingest scanner marked briefing_blocked.
        conn.execute(
            "INSERT INTO entities (
                id, plugin_id, kind, name, short_name, source_file_path, properties,
                content_hash, created_at, updated_at
             ) VALUES (
                ?1, 'core', 'file', 'secret.py', 'secret.py', ?2, ?3, ?4,
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             )",
            rusqlite::params![
                "core:file:secret.py",
                path.to_string_lossy(),
                r#"{"briefing_blocked":"secret detected by pre-ingest scanner"}"#,
                "hash-anchor",
            ],
        )
        .expect("seed briefing-blocked file anchor");

        // The child entity is clean at the entity level (properties "{}"); the
        // block must come from the resolved file anchor, not the entity itself.
        let mut entity = entity_row("python:function:demo.secret", "secret", None);
        entity.source_file_path = Some(path.to_string_lossy().into_owned());
        entity.source_file_id = Some("core:file:secret.py".to_owned());
        entity.source_line_start = Some(1);
        entity.source_line_end = Some(1);

        let out = super::source_for_entity_json(&conn, &entity, 10);

        assert_eq!(out["source_status"], "briefing_blocked");
        assert_eq!(
            out["briefing_blocked"],
            "secret detected by pre-ingest scanner"
        );
        assert!(out.get("lines").is_none(), "must not return lines: {out}");
        assert!(
            !out.to_string().contains("super-secret-value"),
            "leaked briefing-blocked bytes via file anchor: {out}"
        );
    }

    struct BlockingProvider {
        release: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<()>>,
        started: Option<tokio::sync::mpsc::Sender<()>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for BlockingProvider {
        fn name(&self) -> &'static str {
            "blocking"
        }

        async fn invoke(&self, _request: LlmRequest) -> Result<LlmResponse, LlmProviderError> {
            if let Some(started) = &self.started {
                let _ = started.send(()).await;
            }
            let mut rx = self.release.lock().await;
            let _ = rx.recv().await;
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
