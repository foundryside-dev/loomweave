# RC1 Architecture Diagrams

These diagrams are text-native so they remain reviewable in the implementation
archive.

## C4 Context

```mermaid
flowchart LR
    Operator[Local operator]
    Agent[Consult-mode agent / MCP client]
    Sibling[Sibling Loom products]
    Provider[Optional LLM providers]
    Repo[Target repository]
    Clarion[Clarion local-first code archaeology]
    DB[(SQLite graph)]

    Operator -->|install / analyze / serve| Clarion
    Clarion -->|walks and analyzes| Repo
    Clarion -->|stores graph/runs/findings| DB
    Agent -->|MCP JSON-RPC| Clarion
    Sibling -->|HTTP read API| Clarion
    Clarion -->|optional summaries / inferred calls| Provider
```

## Container View

```mermaid
flowchart TB
    CLI[clarion-cli]
    Core[clarion-core]
    Storage[clarion-storage]
    MCP[clarion-mcp]
    Scanner[clarion-scanner]
    Py[Python language plugin]
    Fixture[Fixture plugin]
    DB[(SQLite)]
    HTTP[Federation HTTP read API]
    Agent[MCP client]

    CLI --> Core
    CLI --> Storage
    CLI --> Scanner
    CLI --> MCP
    CLI --> HTTP
    CLI -->|spawns| Py
    Core -->|protocol/contracts| Py
    Core -->|test subprocess| Fixture
    Storage --> DB
    MCP --> Storage
    MCP --> Core
    HTTP --> Storage
    Agent --> MCP
```

## Analyze Pipeline

```mermaid
sequenceDiagram
    participant Op as Operator
    participant CLI as clarion analyze
    participant Scan as Secret scanner
    participant Core as Plugin host
    participant Py as Python plugin
    participant Writer as Storage writer actor
    participant DB as SQLite

    Op->>CLI: clarion analyze
    CLI->>Scan: pre-ingest source scan
    alt secret blocks briefing
        Scan-->>CLI: findings / block summary paths
    else no blocking secret
        Scan-->>CLI: ok / baseline-suppressed findings
    end
    CLI->>Core: discover and spawn plugins
    Core->>Py: initialize / analyze_file
    Py-->>Core: entities / edges / findings
    Core-->>CLI: validated plugin output
    CLI->>Writer: insert core:file rows
    CLI->>Writer: insert plugin graph rows
    CLI->>Writer: graph completion and subsystem clustering rows
    Writer->>DB: serialized commits
    CLI->>Writer: complete / soft_fail / hard_fail run
```

## Serve And Query Flow

```mermaid
flowchart LR
    DB[(SQLite graph)]
    Reader[ReaderPool]
    MCP[clarion-mcp tools]
    HTTP[HTTP read API]
    Agent[MCP client]
    Sibling[Filigree/Wardline consumer]
    LLM[Optional LLM provider]
    Writer[Writer actor]

    DB --> Reader
    Reader --> MCP
    Reader --> HTTP
    Agent --> MCP
    Sibling --> HTTP
    MCP -->|summary/inferred cache miss| LLM
    MCP -->|cache/finding writes| Writer
    Writer --> DB
```

## Trust Boundaries

```mermaid
flowchart TB
    Source[Target source files]
    Scanner[Pre-ingest scanner]
    Plugin[External plugin subprocess]
    Host[Clarion plugin host]
    DB[(SQLite)]
    MCP[MCP stdio]
    HTTP[HTTP read API]
    LLM[External LLM/CLI provider]

    Source --> Scanner
    Scanner -->|allowed or briefing-blocked| Host
    Host -->|Content-Length JSON-RPC| Plugin
    Plugin -->|untrusted entities/edges/findings| Host
    Host -->|validated graph writes| DB
    DB --> MCP
    DB --> HTTP
    MCP -->|hash-checked excerpts| LLM
```

## Release Lane

```mermaid
flowchart LR
    RC1[RC1 branch]
    CI[CI floor]
    Gov[Release governance guard]
    Dry[Release dry run]
    Tag[v* tag]
    Assets[GitHub Release assets]
    Smoke[Artifact smoke test]
    Sign[Checksums / cosign / SLSA provenance]

    RC1 --> CI
    CI --> Gov
    Gov --> Dry
    Dry --> Tag
    Tag --> Assets
    Assets --> Smoke
    Smoke --> Sign
```

## Notes

- Clarion has one durable local graph store. Sibling products enrich Clarion
  through APIs, not shared runtime.
- The strongest architectural boundary is the storage writer actor: durable
  mutation is serialized while reads are pooled.
- The most sensitive data boundary is source-to-LLM; scanner blocking, source
  hashes, live-provider opt-in, and token budgets all participate in this
  control.
