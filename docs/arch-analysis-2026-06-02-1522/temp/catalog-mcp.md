## clarion-mcp — MCP Consult Surface

**Location:** `crates/clarion-mcp/`

**Responsibility:** Implements the full MCP (Model Context Protocol) JSON-RPC tool surface that consult-mode LLM agents call to query Clarion's entity/graph index; owns the stdio transport (dual-framing autodetect), ServerState dispatcher, BudgetLedger, the WS5 catalogue subdir (faceted search + exploration-elimination shortcuts), snapshot resource, index_diff, scan_results emission, Filigree HTTP client, and Wardline qualname-reconciliation logic.

---

### Key Components

- `src/lib.rs` (7,101 LOC) — `list_tools()` (tools 1–35, `lib.rs:104`); `ServerState` struct + all per-tool `tool_*` handlers (dispatcher entry: `lib.rs:729–870`); framing autodetect (`peek_stdio_frame_start`, `lib.rs:~2757`); `BudgetLedger` + `BudgetReservation` (`lib.rs:~2237`); inferred-edge coalescer `InferredInflight` + `InferredInflightGuard` (`lib.rs:90, ~2438`); `entity_json` SEI-injecting serialiser (`lib.rs:4804`); unit tests inline (`lib.rs:~7038+`).
- `src/catalogue/mod.rs` (427 LOC) — `Page`, `paginate`, `finalize_entity_page`, `ScopeFilter`, `RawScope`, `glob_match`; shared paging + scope-filter infrastructure used by all WS5 tools; `missing_signal` honest-empty note constructor. Entry: `catalogue/mod.rs:1`.
- `src/catalogue/shortcuts.rs` (683 LOC) — two families: (1) on-demand graph queries: `tool_find_circular_imports` (SCC over import edges) and `tool_find_coupling_hotspots` (fan-in+fan-out ranking; `EDGE_SCAN_CAP = 500_000`, `shortcuts.rs:29`); (2) honest-empty categorisation/churn shortcuts reusing `tag_facet` from `faceted.rs`: `tool_find_entry_points`, `tool_find_http_routes`, `tool_find_data_models`, `tool_find_tests`, `tool_find_deprecations`, `tool_find_todos`, `tool_what_tests_this`, `tool_high_churn`, `tool_recently_changed` (`shortcuts.rs:243–683`).
- `src/catalogue/inspection.rs` (551 LOC) — `tool_guidance_for` (scope_rank-sorted composition: explicit `guides` edges + `match_rules` rules covering path/tag/kind/subsystem/entity; expiry-filtered; `wardline_group` rules reported but not evaluated against opaque blob; `inspection.rs:47`), `tool_findings_for` (filtered by kind/severity/status), `tool_wardline_for` (verbatim taint-blob with `result_kind: present | no_facts`). SEI-bearing via `entity_json`. Bounds: `GUIDANCE_SCAN_CAP=2000`, `FINDINGS_SCAN_CAP=5000`.
- `src/catalogue/faceted.rs` (275 LOC) — three pure faceted-search tools: `tool_find_by_tag` (tag facet over `entities_by_tag`), `tool_find_by_kind` (kind facet over `entities_by_kind`), `tool_find_by_wardline` (Wardline taint facts with best-effort tier/group filter on opaque blob; `faceted.rs:28`); also defines shared `tag_facet` helper (with `missing_signal` injection) reused by `shortcuts.rs`.
- `src/snapshot.rs` (858 LOC) — `ProjectSnapshot`, `Staleness` enum, `project_snapshot`, `unreadable_db_snapshot`; serves the `clarion://context` MCP resource; two-pass freshness algorithm (structural drift via parent-dir mtime + bounded modification scan, `snapshot.rs:19–74`).
- `src/index_diff.rs` (697 LOC) — `tool_index_diff` logic; `gather_git_facts` (read-only subprocess, fail-soft); HEAD-vs-run committer-date comparison; dirty working-tree detection.
- `src/scan_results.rs` (533 LOC) — `ScanResultsRequest`, `PreparedBatch`, `severity_to_wire` mapping (`scan_results.rs:33`); pure request-builder / response-parser for `POST /api/v1/scan-results` (WP9-B, REQ-FINDING-03); no HTTP here — HTTP lives in `filigree.rs`.
- `src/filigree.rs` (1,016 LOC) — `FiligreeLookup` trait (`filigree.rs:144`); `FiligreeHttpClient` with `reqwest::blocking::Client` (`filigree.rs:176`); `associations_for`, `issue_detail`, `wardline_findings_for_path`, `post_scan_results`, `post_clean_stale`; response shapes (`EntityAssociationsResponse`, `WardlineFinding`, `WardlineFindingsResponse`, `LoomFilesResponse`, `IssueDetail`).
- `src/wardline_reconcile.rs` (155 LOC) — `reconcile_for_entity`; qualname-match (`metadata.wardline.qualname` vs entity-id segment-3); `ResolutionConfidence::{Exact, Heuristic, None}`; `Heuristic` variant reserved for future best-effort normalization (`wardline_reconcile.rs:16`).
- `src/analyze_runs.rs` (256 LOC) — `RunHandle`, `RunRegistry` (`Arc<Mutex<HashMap<String, RunHandle>>>`); `spawn_analyze_child` spawns as process-group leader for group-kill on cancel.
- `src/config.rs` (1,032 LOC) — `McpConfig` (YAML serde_norway); `LlmConfig`, `FiligreeConfig`, `HttpReadConfig`; `select_provider_with_env`; stable `CLA-CONFIG-*` error codes.
- `src/filigree_url.rs` (212 LOC) — URL builders for all Filigree HTTP routes used by clarion-mcp.

---

### Current Tool List (35 tools — dispatch match `lib.rs:729–870`)

**Navigation / entity lookup (8):**
1. `entity_at` — innermost entity at (file, line) + context evidence
2. `find_entity` — paginated FTS over entity id/name/summary
3. `callers_of` — callers with confidence tier (resolved/ambiguous/inferred)
4. `execution_paths_from` — bounded call-graph traversal (max_depth, edge cap)
5. `neighborhood` — one-hop graph (callers, callees, container, contained, refs, imports)
6. `subsystem_members` — modules in a subsystem
7. `subsystem_of` — reverse: which subsystem does this entity belong to
8. `call_sites` — raw source evidence behind call/reference edges

**Enrichment / inspection (6):**
9. `summary` — on-demand cached leaf summary (LLM or structural fallback)
10. `summary_preview_cost` — cost preview before LLM dispatch
11. `source_for_entity` — indexed source span + context window
12. `guidance_for` — composed guidance sheets ranked by scope_rank
13. `findings_for` — findings anchored to entity (filterable by kind/severity/status)
14. `wardline_for` — Wardline taint metadata (opaque blob), verbatim

**Composite / status (3):**
15. `orientation_pack` — deterministic first-pass packet (entity + neighbors + issues + Wardline + health)
16. `project_status` — index freshness, counts, LLM policy, Filigree routing
17. `issues_for` — Filigree associations + Wardline findings reconciled to entity

**Analyze lifecycle (3):**
18. `analyze_start` — spawn background `clarion analyze` child
19. `analyze_status` — poll child progress
20. `analyze_cancel` — SIGKILL process group + mark terminal

**Delta / freshness (1):**
21. `index_diff` — HEAD-vs-analyze staleness, modified/missing files, dirty tree

**Faceted search (`catalogue/faceted.rs`) (3):**
22. `find_by_tag` — entities carrying a plugin-emitted categorisation tag
23. `find_by_kind` — entities of a declared kind
24. `find_by_wardline` — entities with Wardline taint facts (optional tier/group filter on opaque blob)

**Exploration-elimination shortcuts (`catalogue/shortcuts.rs`) (11):**
25. `find_circular_imports` — SCC over import edges (on-demand, scoped, confidence-tiered)
26. `find_coupling_hotspots` — fan-in + fan-out ranking (on-demand, scoped, confidence-tiered)
27. `find_entry_points` — `entry-point` tag (HONEST-EMPTY: not emitted by v1.0 plugins)
28. `find_http_routes` — `http-route` tag (HONEST-EMPTY)
29. `find_data_models` — `data-model` tag (HONEST-EMPTY)
30. `find_tests` — `test` tag (HONEST-EMPTY)
31. `find_deprecations` — `deprecated` tag (HONEST-EMPTY)
32. `find_todos` — `todo` categorisation tag (HONEST-EMPTY)
33. `what_tests_this` — test-tagged callers of entity
34. `high_churn` — entities by `git_churn_count` (HONEST-EMPTY in v1.0; churn not yet indexed)
35. `recently_changed` — per-entity git timestamp (HONEST NO-OP in v1.0; timestamp not indexed)

**Total: 35 tools** (up from 19 in the prior 2026-05-22 analysis; +16 net additions).

---

### Dependencies

**Inbound:**
- `crates/clarion-cli/src/serve.rs` — sole production caller; builds `ServerState`, wires `with_summary_llm` + `with_filigree_client`, drives `serve_stdio_with_state_on_runtime`.
- `crates/clarion-cli/src/http_read.rs` — reuses `clarion_mcp::config::HttpReadConfig`.
- `crates/clarion-cli/src/install.rs` — reads `CLARION_WORKFLOW_SKILL` constant and default actor string.

**Outbound (Rust crates):**
- `clarion-core` — `LlmProvider`, `LlmRequest`, `LlmResponse`, `EdgeConfidence`, prompt builders, `McpErrorCode`, JSON-RPC frame I/O types.
- `clarion-storage` — `ReaderPool::with_reader` (~30 call sites); `WriterCmd` over `mpsc` (3 variants: `InsertInferredEdges`, `TouchSummaryCache`, `UpsertSummaryCache`); all storage functions imported at top of `lib.rs:30–41`.
- `reqwest::blocking` — Filigree HTTP client (`filigree.rs:180`); all calls wrapped in `tokio::task::spawn_blocking` at call sites (`lib.rs:1345, 1395, 1425`).
- `tokio` — current-thread runtime, `AsyncMutex`, `mpsc`, `oneshot`, `broadcast`, `spawn_blocking`.
- `serde_norway` — YAML config deserialization.
- `time`, `blake3`, `thiserror`, `tracing`.

**External services:**
- SQLite via `ReaderPool` (read) + `WriterCmd` channel (write).
- Subprocess `clarion analyze` via `std::process::Command` + process-group SIGKILL.
- Filigree HTTP: `GET /api[/p/{key}]/entity-associations`, `GET /api/loom/issues/{id}`, `GET /api/loom/findings`, `GET /api/loom/files`, `POST /api/v1/scan-results`, `POST /api/v1/findings/clean-stale`.
- LLM providers: `Arc<dyn LlmProvider>` (OpenRouter, Codex CLI, Claude CLI, Recording fixture).

---

### Patterns Observed

- **Stateless per-call dispatch (WS5 invariant)** — All 35 tools are stateless: each call is self-contained with explicit ids/scopes. The cursor-session model documented in §8 is NOT implemented; `ServerState` holds no per-session cursor or breadcrumbs. `catalogue/mod.rs:1–23` documents the invariant explicitly.
- **Dual-framing autodetect** — `peek_stdio_frame_start` (`lib.rs:~2757`) examines the first non-whitespace byte to choose LSP `Content-Length` vs bare JSON-line framing per frame; no configuration required.
- **SEI-carrying entity serialisation** — `entity_json` (`lib.rs:4804`) injects `sei` via `sei_for_locator` join on every entity row emitted by any tool; degrades to JSON `null` on a pre-SEI database without failing the tool.
- **Honest-empty shortcuts (HONEST-EMPTY)** — WS5 categorisation tools (`find_entry_points`, `find_tests`, etc.) return empty results with a `missing_signal` block (`catalogue/mod.rs:134`) rather than fabricating absent signals. `high_churn` and `recently_changed` are HONEST NO-OPs in v1.0.
- **Enrich-only Wardline/Filigree reconciliation** — `issues_for` and `orientation_pack` append a `wardline_findings` section by calling `wardline_section_for_entity` inside `spawn_blocking` (`lib.rs:1425`); a fetch failure degrades to `unavailable`, never fails the tool. This satisfies the Loom federation enrich-only axiom.
- **Capability gating via `Option<…>` builder fields** — LLM (`with_summary_llm`) and Filigree (`with_filigree_client`) features are off until wired; handlers return policy envelopes (`llm-disabled`, `unavailable`) not errors.
- **BudgetLedger + RAII guards** — `BudgetReservation::Drop` releases reserved tokens even on future cancellation (`lib.rs:~2278`); `InferredInflightGuard::Drop` deregisters broadcast senders on cancellation (`lib.rs:~2471`).
- **Scope/page shared infrastructure** — `catalogue/mod.rs` provides `Page`, `ScopeFilter`, `RawScope`, `finalize_entity_page`, `paginate` used uniformly across all 14 catalogue tools; adds `scope_truncated` + `scan_truncated` flags to every response so truncation is never silent.
- **`reqwest::blocking` fully wrapped in `spawn_blocking`** — prior concern resolved: `associations_for` (`lib.rs:1345`), `issue_detail` (`lib.rs:1395`), and `wardline_findings_for_path` (`lib.rs:1425`) each call blocking reqwest inside `tokio::task::spawn_blocking`.

---

### Drift — Code vs. §8/§7/Detailed-Design Tool Catalogue

**Primary drift: §8 and detailed-design §6 tool-count notes are false.** `system-design.md §8:773` states "v1.0 ships an 8-tool subset" and `§8:791` states "exploration-elimination shortcuts deferred to v1.1." Both were accurate at an earlier milestone but were not updated after WS5. The shipped binary has **35 tools** (dispatch `lib.rs:729–870`) including all 14 shortcuts and the full read-enrichment trio (`guidance_for`, `findings_for`, `wardline_for`). Similarly, `detailed-design.md §6:1121` lists a cursor-model tool set (`goto`, `back`, `zoom_out`, `search_structural`, `search_semantic`, `list_findings`, etc.) that describes the v1.1 design target, not the v1.0 actual surface. Neither document has an up-to-date list of the 35 shipped tools.

**Cursor/session model: correctly labeled v1.1 target, not drift.** §8 documents cursor, breadcrumbs, scope_lens, consent gates, and write-effect tools (`emit_observation`, `propose_guidance`) — but §8:773 explicitly labels this as the v1.1 target not present in v1.0. The shipped code (stateless per `catalogue/mod.rs:1`; `ServerState` holding `ReaderPool`, LLM state, Filigree client, AnalyzeProcess, BudgetLedger — no cursor, no breadcrumbs) matches that labeling. This is not drift; it is correctly flagged future work. The actionable part is that the "8-tool v1.0 subset" note adjacent to it IS stale.

**Detailed-design §6 tool catalogue does not enumerate shipped tools.** The 35 shipped tools are absent from `detailed-design.md §6`. A doc consumer reading it will be misled about what the MCP surface actually is. The entire §6 catalogue needs to be replaced or supplemented with the v1.0 actual tool list.

**SEI (ADR-038) not mentioned in §8.** The SEI (`sei_for_locator` injected by `entity_json`, `lib.rs:4804`) is a cross-cutting response property on every entity-returning tool. §8 response shape (`lib.rs:799`) does not document it. `catalogue/mod.rs:16` references ADR-038.

**`scan_results.rs` / WP9-B presence.** `scan_results.rs` (533 LOC, Filigree finding emission) is present in the MCP crate, consistent with docs noting WP9-B as deferred from v1.0 but with scaffolding in place. The code is pure request-builder; the HTTP call lives in `filigree.rs`. No MCP tool exposes scan-results emission directly (it is called from `clarion analyze`, not from the MCP surface).

**`guidance_for` vs §7 Guidance System: partial alignment.** The shipped `tool_guidance_for` (`catalogue/inspection.rs:47`) implements the read side of §7's composition algorithm: scope_rank-ordered query, explicit `guides` edges + `match_rules` (path/tag/kind/subsystem/entity rules), expiry filtering, `wardline_group` rules reported but not evaluated (matching §7: "wardline_group rules are not evaluated here"). The §7 full 7-step composition pipeline (budget-gated rendering into prompt segments, fingerprint, token budget fill inner→outer) is for LLM context enrichment during summarization, not surfaced as a tool. The tool exposes the sheet list, not the rendered prompt. This is correct scope for a read-only MCP tool; the write-effect tools (`propose_guidance`, `promote_guidance`) from §8 are not present — matching the WS5 read-only scope (`catalogue/mod.rs:1: "authoring (WS6) is out of scope"`).

**Blocking reqwest concern (prior analysis): resolved.** Prior catalog (2026-05-22) flagged `reqwest::blocking` called directly in async handlers. All three call sites now use `tokio::task::spawn_blocking` (`lib.rs:1345, 1395, 1425`). Concern closed.

---

### Quality Concerns / Debt

**[HIGH] `lib.rs` 7,101 LOC — dominant change-risk.** A single file contains the tool registry, all 19+ handlers not extracted to `catalogue/`, transport framing, BudgetLedger, inferred-edge coalescer, analyze-process management, entity_json, and ~700 LOC of inline tests. Merge conflicts and slow IDE feedback are active pain. The WS5 catalogue subdir extracted 17 tools into 4 files (1,936 LOC total): 3 in `faceted.rs`, 11 in `shortcuts.rs`, and 3 in `inspection.rs`. The 18 remaining tools still live in `lib.rs` alongside all non-tool infrastructure. A `tools/` subdir split was noted as desired work (`clarion-42cbd8a25a` referenced in task brief); as of this analysis it is partially done (catalogue/) but not complete. Fix: migrate `tool_entity_at`, `tool_callers_of`, `tool_summary`, `tool_orientation_pack`, etc. into per-family modules under `catalogue/` or a new `tools/` subdir; move transport to `transport.rs`; move LLM dispatch machinery to `dispatch.rs`.

**[MEDIUM] §8 and detailed-design §6 are significantly stale.** The cursor/session model in §8 is not implemented. The §8 "8-tool v1.0 subset" and "shortcuts deferred to v1.1" notes are false. The detailed-design §6 catalogue lists ~30+ tools that do not exist and omits the 35 that do. A design consumer reading these docs will be misled about what Clarion's MCP surface actually is. Fix: update §8's tool list paragraph and the §8 "v1.0 ships" note; add a brief actual tool catalogue there; defer the cursor/session material explicitly to §12 backlog.

**[MEDIUM] `analyze_runs.rs` stale-running-row reconciliation deferred.** `analyze_runs.rs:13–16` notes that stale `running` rows from a supervising-process crash are "explicitly out of scope" pending `owner_pid`/`heartbeat_at` work (issue `clarion-f9027d2187`). A Clarion server crash leaves the DB with a permanently `running` analyze row, which blocks future `analyze_start` calls (cross-process lock). Fix: on `analyze_start`, detect rows stuck in `running` with a stale heartbeat and mark them failed.

**[MEDIUM] Sequential stdio dispatch.** The server processes one frame at a time (`runtime.block_on` inside the read loop). A slow `summary` (LLM-dispatching, potentially minutes) blocks all subsequent tool calls on the same stdio session. Fine for single-agent use; problematic for multi-agent or tools that could run concurrently (e.g., multiple `find_*` calls). Fix: concurrent frame dispatch with response ordering would require framed multiplexing; probably a v1.1 concern.

**[LOW] `recently_changed` is a documented NO-OP.** The tool registry description (`lib.rs:422`) and the tool implementation both state that per-entity git change timestamps are not indexed in v1.0, so the tool always returns empty with a `missing_signal` note. The tool exists as a future wire stub. Low severity because it is honest about this in the tool description and response.

**[LOW] No timeout on `analyze_start` child.** The spawned `clarion analyze` child runs unbounded; `analyze_cancel` is the only stop. If an agent starts analyze and never polls or cancels, the child outlives the MCP session. Fix: add a configurable wall-clock limit; send SIGKILL and mark failed if exceeded.

**[LOW] Mutex poisoning swallowed everywhere.** `analyze_process.lock()`, `budget.lock()`, etc. use `.unwrap_or_else(std::sync::PoisonError::into_inner)` (e.g., `lib.rs:~556`, `~2061`, `~2284`). This silently masks the panic that caused poisoning. Acceptable as a "keep serving" policy choice, but uncommented.

**[LOW] `catalogue/mod.rs` glob_match is custom-rolled.** The `glob_match` / `segment_match` implementation (427 lines, `catalogue/mod.rs:146–184`) is a hand-written glob engine with unit tests. No crate dependency. Correct for the documented patterns (`**`, `*`, `?`), but path-separator semantics and edge cases (leading `**/`, empty segments) have no normative spec other than the tests. Low risk given the test coverage; noted for awareness.

---

### Confidence

High — Read `lib.rs` dispatch match (lines 104–870) end-to-end (full tool registry + dispatch table); sampled handlers at `lib.rs:1297–1434` (issues_for + spawn_blocking fix verification), `lib.rs:4800–4813` (entity_json/SEI), `lib.rs:4545–4584` (wardline_section_for_entity). Read `catalogue/mod.rs` (427 lines, full), `catalogue/faceted.rs` (275 lines, full), `catalogue/shortcuts.rs` (full, 683 lines), `catalogue/inspection.rs` (1–100 with handler and guidance composition verified), `wardline_reconcile.rs` (full, 155 lines), `snapshot.rs` (1–80), `index_diff.rs` (1–80), `scan_results.rs` (1–80), `filigree.rs` (1–300, through trait + client impl). Read `system-design.md §7` (lines 662–714, guidance composition algorithm), `system-design.md §8` (lines 747–846), and `detailed-design.md §6` (lines 1121–1182) in full. Confirmed tool count by counting dispatch-match arms (35, `lib.rs:729–870`). Cross-verified blocking-reqwest fix at all three `spawn_blocking` sites. Cross-verified cursor/session absence by reading `ServerState` fields. Corrected module/handler assignment for `shortcuts.rs` vs `faceted.rs` after reading both files in full.
