# 03 — Architecture Diagrams

**Repository:** `/home/john/clarion`
**Branch:** `sprint-2/b8-scale-test`
**Generated:** 2026-05-18

All diagrams are Mermaid, validated through the Mermaid Live renderer. They draw from `01-discovery-findings.md` and `02-subsystem-catalog.md`.

Five views, in the C4-inspired order — context, container, then two narrative sequences and one component zoom.

---

## 1 — System Context (C4 L1): Clarion in the Loom federation

Clarion-as-a-system, the actors and the external systems it talks to. The two solid blue siblings (Filigree, Wardline) are owned by the same author and explicitly enrich-only / soft-import per `docs/suite/loom.md` §3–§5. The dashed line to Wardline marks the named v0.1 doctrine asterisk.

```mermaid
flowchart LR
    classDef person fill:#1168bd,color:#fff,stroke:#0b4884
    classDef softwareSystem fill:#1168bd,color:#fff,stroke:#0b4884
    classDef external fill:#999999,color:#fff,stroke:#666666
    classDef sibling fill:#438dd5,color:#fff,stroke:#2e6295

    OP["Operator<br/>(developer / CI)"]:::person
    AGENT["Consult-mode LLM agent<br/>(via MCP client)"]:::person

    CLARION["<b>Clarion</b><br/>Code-archaeology service<br/>Rust core + language plugins<br/>SQLite-backed local state"]:::softwareSystem

    PYRIGHT["pyright-langserver<br/>(LSP subprocess)"]:::external
    OPENROUTER["OpenRouter HTTP API<br/>(LLM provider)"]:::external

    FILIGREE["<b>Filigree</b><br/>Issue tracker<br/>(Loom sibling — enrich only)"]:::sibling
    WARDLINE["<b>Wardline</b><br/>Static checks + fingerprints<br/>(Loom sibling — soft import)"]:::sibling

    SOURCE[("Source tree<br/>(project_root)")]

    OP -->|"clarion install / analyze / serve"| CLARION
    AGENT -->|"MCP stdio<br/>2025-11-25"| CLARION
    CLARION -->|"reads project files<br/>under jail"| SOURCE
    CLARION -->|"spawns LSP subprocess<br/>(per Python plugin)"| PYRIGHT
    CLARION -->|"HTTPS chat/completions<br/>strict-JSON, budget-bounded"| OPENROUTER
    CLARION -->|"GET /api/entity-associations<br/>(enrich-only)"| FILIGREE
    CLARION -.->|"import probe at handshake<br/>(asterisk: loom.md §5)"| WARDLINE
```

**Key facts:**
- One operator + one MCP client are the primary actors.
- Two outbound HTTP integrations (OpenRouter, Filigree) plus one subprocess (pyright). Wardline is import-only.
- The MCP wire is stdio, not HTTP — `clarion serve` runs in the foreground of the agent's process tree.

---

## 2 — Container view (C4 L2): inside the binary

The `clarion` binary is the only deployment artifact. Five workspace crates inside it; two subprocess-protocol peers (one real Python plugin, one test fixture); three external endpoints; two persistent files under `.clarion/`.

```mermaid
flowchart TB
    classDef binary fill:#1168bd,color:#fff,stroke:#0b4884
    classDef crate fill:#85bbf0,color:#000,stroke:#5d82a8
    classDef subproc fill:#fff2cc,color:#000,stroke:#d6b656
    classDef datastore fill:#cccccc,color:#000,stroke:#666
    classDef external fill:#999999,color:#fff,stroke:#666

    subgraph CLARION_HOST["clarion binary (single executable)"]
        direction TB
        CLI["clarion-cli<br/>install / analyze / serve"]:::binary
        CORE["clarion-core<br/>entity-ID + PluginHost<br/>LlmProvider + manifest<br/>jail / limits / breaker"]:::crate
        STORAGE["clarion-storage<br/>writer-actor (sole rusqlite)<br/>deadpool reader pool<br/>9 WriterCmd variants"]:::crate
        MCP["clarion-mcp<br/>MCP 2025-11-25<br/>7 read tools<br/>5-tuple cache + budget"]:::crate
    end

    PYPLUGIN["plugins/python<br/>clarion-plugin-python<br/>JSON-RPC subprocess"]:::subproc
    FIXTURE["clarion-plugin-fixture<br/>(test-only)"]:::subproc

    PYRIGHT["pyright-langserver"]:::external
    OPENROUTER["OpenRouter HTTP"]:::external
    FILIGREE_HTTP["Filigree HTTP"]:::external

    DB[(".clarion/clarion.db<br/>SQLite WAL")]:::datastore
    YAML[(".clarion/clarion.yaml<br/>operator config")]:::datastore

    AGENT["LLM agent (MCP client)"]
    OP["Operator (shell / CI)"]

    OP -->|"argv"| CLI
    AGENT -->|"stdio JSON-RPC"| CLI
    CLI --> CORE
    CLI --> STORAGE
    CLI --> MCP
    MCP --> CORE
    MCP --> STORAGE
    STORAGE --> CORE
    CLI -->|"discover + spawn"| PYPLUGIN
    CLI -.->|"spawn (tests only)"| FIXTURE
    PYPLUGIN -->|"spawns once<br/>per session"| PYRIGHT
    MCP -->|"strict-JSON"| OPENROUTER
    MCP -->|"GET enrich"| FILIGREE_HTTP
    STORAGE -->|"writer + readers"| DB
    CLI -->|"writes on install"| DB
    CLI -->|"reads on serve"| YAML
```

**Things worth noting:**
- The Rust crate graph is acyclic: `cli` → {`mcp`, `storage`, `core`}; `mcp` → {`storage`, `core`}; `storage` → `core` (one symbol, `EdgeConfidence`).
- The Python plugin is *not* a Rust crate; the only "dep" is the host-spawn-subprocess contract.
- The MCP server speaks a separate stdio session from the analyze run. They can be running concurrently against the same `.clarion/clarion.db`; correctness relies entirely on `clarion-storage`'s WAL + `busy_timeout=5000` pragma discipline.

---

## 3 — `clarion analyze` run (sequence)

The happy path and the two named failure modes (`SoftFailed`, `HardFailed`). The `RunOutcome` taxonomy is at `crates/clarion-cli/src/analyze.rs:558–563`; the three terminal branches differ by *which* `WriterCmd` they send.

```mermaid
sequenceDiagram
    autonumber
    participant OP as Operator
    participant CLI as clarion-cli<br/>analyze::run
    participant CORE as clarion-core<br/>PluginHost
    participant PLUGIN as Plugin subprocess<br/>(Python or fixture)
    participant WRITER as clarion-storage<br/>writer-actor
    participant DB as .clarion/clarion.db

    OP->>CLI: clarion analyze [PATH]
    CLI->>WRITER: Writer::spawn(db_path)
    CLI->>WRITER: BeginRun(run_id)
    WRITER->>DB: INSERT runs (status=running) + BEGIN
    Note over CLI: discover plugins on $PATH<br/>(clarion-plugin-*)
    CLI->>CORE: discover()
    loop per plugin
        CLI->>CORE: PluginHost::spawn (pre_exec setrlimit)
        CORE->>PLUGIN: stdin/stdout pipes
        CORE->>PLUGIN: initialize (handshake)
        PLUGIN-->>CORE: InitializeResult + capabilities
        CORE->>PLUGIN: initialized
        loop per file in plugin extensions
            CLI->>CORE: host.analyze_file(path)
            CORE->>PLUGIN: analyze_file
            PLUGIN-->>CORE: entities + edges + stats
            Note over CORE: 5-step validator pipeline<br/>field-size / kind / id / jail / cap
            CORE-->>CLI: AcceptedEntity/Edge + HostFinding
        end
        CLI->>WRITER: InsertEntity * N
        CLI->>WRITER: ReplaceUnresolvedCallSitesForCaller * N
        CLI->>WRITER: InsertEdge * N (enforce_edge_contract)
        WRITER->>DB: batched INSERTs (cadence=50)
        CLI->>CORE: host.shutdown / kill / reap
    end
    alt all plugins OK
        CLI->>WRITER: CommitRun(Completed, stats_json)
        WRITER->>DB: UPDATE runs SET status='completed' + COMMIT
    else some plugin crashed (SoftFailed)
        CLI->>WRITER: CommitRun(Failed, failure_reason)
        WRITER->>DB: UPDATE runs SET status='failed' + COMMIT (partial work kept)
    else writer error (HardFailed)
        CLI->>WRITER: FailRun(reason)
        WRITER->>DB: ROLLBACK + UPDATE runs SET status='failed'
    end
    CLI-->>OP: exit code (0 / nonzero)
```

**Key invariants:**
- All writes funnel through a single writer-actor task that owns the sole `rusqlite::Connection` (ADR-011).
- The five-step host validator pipeline is the place where plugin-side guarantees become host-side facts: field-size, kind-declared, entity-id-matches, path-jail, entity-cap. Steps 0–2 only drop offending records; steps 3–4 escalate to plugin termination on breaker trip.
- The `SoftFailed` branch is the one path where the same SQLite transaction carries both accepted entities *and* a `UPDATE runs SET status='failed'` — a partial-work-with-marker invariant.

---

## 4 — MCP `summary` tool with cache miss → LLM dispatch (sequence)

The richest narrative path in the codebase: the ADR-007 5-tuple cache lookup, budget reservation, `spawn_blocking` to the synchronous `LlmProvider`, JSON-shape validation, and writeback via the writer-actor.

```mermaid
sequenceDiagram
    autonumber
    participant AGENT as MCP client<br/>(LLM agent)
    participant MCP as clarion-mcp<br/>tool_summary
    participant READER as clarion-storage<br/>ReaderPool
    participant CACHE as summary_cache<br/>(SQLite)
    participant BUDGET as BudgetLedger<br/>(in-memory)
    participant PROV as OpenRouter<br/>(LlmProvider)
    participant WRITER as writer-actor

    AGENT->>MCP: tools/call summary { id }
    MCP->>READER: with_reader → entity_by_id + summary_cache_lookup
    READER->>CACHE: SELECT by 5-tuple<br/>(entity_id, content_hash, prompt_template_id,<br/> model_tier, guidance_fingerprint)
    alt cache hit and not stale and not expired
        CACHE-->>MCP: SummaryCacheEntry
        MCP->>WRITER: TouchSummaryCache(last_accessed_at)
        MCP-->>AGENT: summary envelope (cached=true)
    else cache miss / stale_semantic / >180 days
        MCP->>BUDGET: reserve_budget (pessimistic token estimate)
        alt budget exhausted
            BUDGET-->>MCP: token-ceiling-exceeded (sticky)
            MCP-->>AGENT: ok=false, retryable=false
        else budget OK
            BUDGET-->>MCP: BudgetReservation (RAII)
            Note over MCP: build_leaf_summary_prompt<br/>(LEAF_SUMMARY_PROMPT_TEMPLATE_ID = "leaf-v1")
            MCP->>PROV: spawn_blocking → invoke(LlmRequest)<br/>response_format strict-JSON
            PROV-->>MCP: LlmResponse (text + usage)
            MCP->>BUDGET: commit(usage.input + output)
            MCP->>WRITER: UpsertSummaryCache(SummaryCacheKey, payload)
            WRITER-->>MCP: ack via oneshot
            MCP-->>AGENT: summary envelope (cached=false)
        end
    end
```

**Notes that don't fit on the diagram:**
- The 4-tuple `InferredEdgeCacheKey` for the inferred-edges path is structurally identical: `(caller_entity_id, caller_content_hash, model_id, prompt_version)`. The flow looks the same except for an additional **in-flight coalescer** (`inferred_inflight: HashMap<InferredEdgeCacheKey, broadcast::Sender>`) with a 60-second timeout so concurrent identical dispatches share one LLM call.
- `BudgetLedger.blocked` is sticky for the lifetime of `ServerState` — once one reservation overshoots, every subsequent LLM tool returns `token-ceiling-exceeded` until process restart. No reset path.
- Filigree's `issues_for` path *also* uses `spawn_blocking` (for `reqwest::blocking`) — same bridging shape, no cache, three independent skip paths route to `issues_unavailable` to honour Loom's enrich-only contract.

---

## 5 — Component zoom: `clarion-core::plugin::host` validator pipeline

The internal organisation of `PluginHost::analyze_file` — the per-file dispatch shape inside the plugin host supervisor. Sourced from `crates/clarion-core/src/plugin/host.rs:1031–1198`.

```mermaid
flowchart TB
    classDef accept fill:#c9e7c9,color:#000,stroke:#5a8a5a
    classDef drop fill:#fff2cc,color:#000,stroke:#d6b656
    classDef kill fill:#f4cccc,color:#000,stroke:#a64d4d
    classDef entry fill:#85bbf0,color:#000,stroke:#5d82a8

    START["host.analyze_file(path)"]:::entry
    SEND["Send analyze_file request<br/>over JSON-RPC + read frame"]:::entry
    OUTCOME["Decoded AnalyzeFileOutcome:<br/>entities, edges, stats"]:::entry

    S0{"0. field-size cap<br/>(4 KiB scalar, 64 KiB extra)"}
    S1{"1. ontology declared-kind<br/>(manifest entity_kinds)"}
    S2{"2. entity-id identity<br/>(canonical_qualified_name ≡ id)"}
    S3{"3. path-jail check<br/>+ PathEscapeBreaker tick"}
    S4{"4. entity-cap check<br/>(plugin-declared budget)"}

    EDGES["process_edges<br/>(drop-only, no kill paths)"]
    STATS["process_stats<br/>(record p95 latency)"]
    OUT_ACCEPT["AcceptedEntity ⇒ caller"]:::accept
    DROP_F["drop record<br/>+ HostFinding"]:::drop
    KILL_J["kill plugin<br/>(disabled-path-escape)"]:::kill
    KILL_C["kill plugin<br/>(entity-cap exceeded)"]:::kill

    START --> SEND --> OUTCOME --> S0
    S0 -->|"oversize"| DROP_F
    S0 -->|"OK"| S1
    S1 -->|"undeclared kind"| DROP_F
    S1 -->|"OK"| S2
    S2 -->|"mismatch"| DROP_F
    S2 -->|"OK"| S3
    S3 -->|"escape + breaker tripped"| KILL_J
    S3 -->|"escape (under threshold)"| DROP_F
    S3 -->|"OK"| S4
    S4 -->|"exceeded"| KILL_C
    S4 -->|"OK"| OUT_ACCEPT
    OUTCOME --> EDGES
    OUTCOME --> STATS
```

**Design notes:**
- Steps 0–2 are **pure-function validators** (`oversize_field`, `oversize_edge_field`, `invalid_unresolved_call_site_reason`, `validate_kind_string`); they emit a `HostFinding` and drop the offending row.
- Steps 3–4 carry **kill paths** because they involve cross-record state: `PathEscapeBreaker` ticks once per offending entity and trips after a documented threshold, and entity-cap is a per-plugin budget that's only meaningful in aggregate.
- The same drop-on-violation discipline is applied to edges, but with **no kill paths** — edges do not participate in breakers. Trade-off: an edge-heavy file can spam many findings; an entity-heavy file is bounded by the cap.

---

## Coverage notes

| C4 level | Diagram | Subsystems covered |
|----------|---------|---------------------|
| L1 Context | #1 | Clarion as a whole, all 5 external dependencies |
| L2 Container | #2 | All 6 internal subsystems + 5 external |
| L3 Component | #5 | `clarion-core::plugin::host` (largest production file) |
| Sequence | #3 | `clarion analyze` lifecycle, all 3 terminal branches |
| Sequence | #4 | MCP `summary` LLM dispatch, ADR-007 5-tuple cache |

What's deliberately not drawn here (size / clarity vs. info-value tradeoff):
- A component view of `clarion-mcp::lib.rs` — it has a clean banded structure (protocol surface → `ServerState` → per-tool handlers → LLM pipelines → transport loop → helpers) but the 7 tools × 4 substates each would dominate a single diagram. The catalog entry's tool table is the substitute.
- A component view of `clarion-storage::writer` — the 9 `WriterCmd` variants are clean and listed in the catalog; visualising the per-variant SQL would obscure rather than illuminate.
- A schema ER diagram — the catalog's ASCII schema sketch is sufficient for v0.1's 8-table footprint.
