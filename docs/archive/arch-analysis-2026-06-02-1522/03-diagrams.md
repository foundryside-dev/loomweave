# 03 — Architecture Diagrams

**Date:** 2026-06-02 · v1.1.0. Mermaid; render in any Mermaid-aware viewer.

---

## C4 Level 1 — System Context

```mermaid
graph TB
    agent["Consult-mode LLM agent<br/>(Claude Code, etc.)"]
    operator["Operator / CI"]
    clarion["<b>Clarion</b><br/>code-archaeology tool<br/>(single Rust binary + Python plugin)"]
    repo[("Target codebase<br/>(e.g. elspeth ~425k LOC)")]
    llm["LLM provider<br/>(OpenRouter / claude CLI / codex CLI)"]
    filigree["Filigree<br/>(issue tracker — sibling)"]
    wardline["Wardline<br/>(trust topology — sibling)"]

    operator -->|"clarion install / analyze / serve"| clarion
    agent -->|"MCP stdio: 35 tools"| clarion
    agent -.->|"HTTP read API (federation)"| clarion
    clarion -->|"reads source"| repo
    clarion -->|"summary() on demand"| llm
    clarion -.->|"enrich-only: issues_for / associations"| filigree
    clarion -.->|"enrich-only: taint facts"| wardline

    classDef sib fill:#eef,stroke:#88a;
    class filigree,wardline sib;
```

Dotted = enrich-only / optional (Loom federation axiom: Clarion is solo-useful; siblings add information, never define semantics).

---

## C4 Level 2 — Containers / Crates

```mermaid
graph TB
    subgraph bin["clarion binary (clarion-cli)"]
        cli["CLI dispatch<br/>install·analyze·serve·hook·doctor"]
        pipeline["Analysis pipeline<br/>analyze.rs (3,542 LOC)"]
        http["HTTP read API<br/>http_read.rs (4,387 LOC)<br/>16 routes"]
        serve["serve supervisor<br/>2 runtimes, 1 ReaderPool"]
    end
    mcp["clarion-mcp<br/>MCP server, 35 tools<br/>lib.rs 7,101 LOC"]
    core["clarion-core<br/>plugin host + entity-ID + LLM provider"]
    storage["clarion-storage<br/>writer-actor + reader-pool<br/>SQLite (13 tables)"]
    scanner["clarion-scanner<br/>secret detection lib"]
    pyplugin["clarion-plugin-python<br/>(subprocess, wire-isolated)"]
    db[("SQLite<br/>.clarion/clarion.db")]

    cli --> pipeline
    cli --> serve
    serve --> mcp
    serve --> http
    pipeline --> core
    pipeline --> storage
    pipeline --> scanner
    mcp --> storage
    mcp --> core
    http --> storage
    storage --> core
    scanner -.no internal deps.-> scanner
    core <-->|"JSON-RPC over pipes<br/>Content-Length framing"| pyplugin
    storage --> db
    http --> db
    mcp --> db
```

---

## Inter-crate dependency DAG (no cycles)

```mermaid
graph LR
    cli[clarion-cli] --> core[clarion-core]
    cli --> storage[clarion-storage]
    cli --> scanner[clarion-scanner]
    cli --> mcp[clarion-mcp]
    mcp --> storage
    mcp --> core
    storage --> core
    fixture[clarion-plugin-fixture] --> core
    python[plugins/python] -. wire only .-> core

    leak["⚠ facade-bypass leak:<br/>writer.rs:537 →<br/>core::plugin::manifest::RESERVED_ENTITY_KINDS"]
    storage -.-> leak
```

`clarion-core` is the DAG bottom; `clarion-storage`/`clarion-scanner` are lower-layer; `clarion-cli`/`clarion-mcp` are consumers. The Python plugin shares no Rust dep.

---

## Component — `clarion analyze` pipeline (current ~9 phases)

```mermaid
sequenceDiagram
    participant CLI as run_with_options
    participant W as writer-actor
    participant Sc as scanner driver
    participant PH as PluginHost
    participant Py as python plugin
    participant Cl as clustering

    CLI->>W: orphan-run recovery (UPDATE runs SET failed WHERE running)
    Note over CLI: Wave-2 incremental file-hash skip
    CLI->>Sc: parallel secret scan (BEFORE BeginRun)
    CLI->>W: BeginRun (mint run_id)
    CLI->>PH: discover $PATH clarion-plugin-*
    loop per plugin (spawn_blocking)
        PH->>Py: initialize / analyze_file × N / shutdown
        Py-->>PH: entities + edges
        Note over PH: 4-stage validation;<br/>PathEscapeBreaker (host)
        Note over CLI: CrashLoopBreaker (run loop)
        PH-->>W: entities → unresolved → edges (FK order)
    end
    CLI->>Cl: Leiden clustering (deterministic seed) + fallback
    CLI->>W: CommitRun(Completed)
    Note over CLI,W: Wave-1 SEI mint pass (post-commit, enrich-only, --no-sei skippable)
```

---

## Component — `clarion serve` topology

```mermaid
graph TB
    subgraph proc["clarion serve (one process)"]
        rt1["current-thread tokio rt"] --> mcpsrv["MCP stdio server<br/>(sequential dispatch)"]
        rt2["multi-thread tokio rt"] --> axum["Axum HTTP read API"]
        pool[("ReaderPool<br/>(shared, Arc::ptr_eq proven)")]
        mcpsrv --> pool
        axum --> pool
        wtaint["optional Wardline taint<br/>writer-actor (ADR-036)"]
        axum --> wtaint
    end
    pool --> db[("SQLite WAL")]
    Note["⚠ either thread crashing kills the binary;<br/>no per-surface restart"]
```

---

## DRIFT map — design doc vs shipped code

```mermaid
graph LR
    subgraph docs["Design docs (LAGGING)"]
        s2["§2 Core/Plugin"]
        s2py["§2 Python specifics"]
        s5["§5 Policy Engine"]
        s6["§6 Pipeline"]
        s8["§8 MCP surface"]
        s9["§9 HTTP API"]
        dd["detailed-design schema"]
        claude["CLAUDE.md layout"]
    end
    s2 -->|"async/mpsc/streaming/file_list<br/>NOT BUILT"| codeSync["code: fully synchronous host"]
    s2py -->|"tree-sitter + LibCST<br/>NOT USED"| codeAst["code: CPython ast only"]
    s5 -->|"Anthropic/cache_control/cost_report<br/>NOT BUILT"| codeProv["code: 4 providers, no budget engine"]
    s6 -->|"4 phase-7 CLA-FACT-* findings<br/>NOT BUILT; 3 SEI phases UNDOCUMENTED"| codePipe["code: ~9 phases, SEI"]
    s8 -->|"'8-tool subset'<br/>FALSE"| codeMcp["code: 35 tools"]
    s9 -->|"entities/resolve shipped<br/>DOES NOT EXIST"| codeHttp["code: 16 routes; contracts.md authoritative"]
    dd -->|"6 tables + FTS5 documented"| codeDb["code: 13 tables + FTS5 + view / 6 migrations"]
    claude -->|"4 crates / v1.0.0<br/>(omits mcp + scanner)"| codeWs["code: 6 crates / v1.1.0"]

    classDef bug fill:#fdd,stroke:#c44;
    class s2,s2py,s5,s6,s8,s9,dd,claude bug;
```

Red = the doc is the bug (per CLAUDE.md precedence: code wins over a contradicting design doc). Triage and code-vs-doc resolution in `06-architect-handover.md`. (ADR-013's GCP-rule "drift" was a validation strawman — doc and code agree — and has been dropped.)
