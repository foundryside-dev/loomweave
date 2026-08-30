# The Weft Suite — A Briefing

**Audience**: engineers, reviewers, or stakeholders new to the Weft suite
**Purpose**: explain what each tool does, how they fit together, and what state the suite is in today
**Reading time**: ~5 minutes

---

## The one-paragraph version

**Weft** is a suite for enterprise-grade code governance on small teams. Its v0.1 products — **Loomweave**, **Filigree**, and **Wardline** — are three independent tools that enrich one another through narrow additive protocols. Each is fully authoritative in its domain and fully usable on its own. Loomweave builds a trustworthy catalog of a codebase and answers structural questions. Filigree tracks the issues, findings, and observations that arise from examining that codebase. Wardline declares and enforces the trust topology that constrains how code is allowed to behave. Together they deliver rigor that normally requires enterprise-scale platform teams — without the operational weight, and without any shared runtime, store, or orchestrator. A fourth product, **Shuttle**, is proposed for transactional scoped change execution; see [weft.md](./weft.md) for the suite's founding doctrine, the enrichment-not-load-bearing principle, and the go/no-go test that governs future products.

> **Authoritative source.** The Weft federation axiom, roster, and composition law are now authoritative at the federation hub: `~/weft/doctrine.md` (as of 2026-06-05). The hub's canonical roster is **5 realized members** — Loomweave, Filigree, Wardline, **Legis**, and **Charter** — plus **Shuttle as a roadmap thought-bubble**; the three-member v0.1 framing in this briefing predates Legis and Charter and is kept as Loomweave's local intro. This briefing remains Loomweave's own introduction to the suite as Loomweave sees it.

---

## The Weft products

### Loomweave — the code-archaeology catalog

**Role**: indexes the source tree and answers structural questions.

Loomweave ingests a codebase, extracts entities (functions, classes, modules, packages), clusters them into subsystems, and produces structured briefings that summarise each entity's purpose, maturity, and relationships. Consult-mode LLM agents query Loomweave through MCP tools so they never need to spawn an explore-agent to answer "what are the entry points?" or "what calls this function?" — Loomweave answered that during its batch analysis and caches the result.

**Authoritative for**: the entity catalog, the code graph, guidance sheets (institutional knowledge attached to entities), and structural / factual findings.

**Typical invocation**: `loomweave analyze <project>` for batch indexing; `loomweave serve` for MCP + HTTP consult.

**Status**: walking skeleton merged (Sprint 1, tagged `v0.1-sprint-1`); v0.1 build in flight against the published design. Target first customer is `elspeth` (~425k LOC Python).

### Filigree — the workflow and findings tracker

**Role**: tracks issues, observations, findings lifecycle, and their triage.

Filigree is where work lives. It holds the project's issues, the observations (fire-and-forget notes) that agents emit during work, the findings that scanners produce, and the lifecycle state of each (open, acknowledged, fixed, suppressed). It exposes an MCP server so agents can query and mutate work items directly, and a dashboard for human operators.

**Authoritative for**: issue state, workflow transitions, observation and finding lifecycle, triage history.

**Typical invocation**: `filigree list`, `filigree create`, `filigree claim-next` from CLI; MCP tools from agents; HTTP dashboard for humans.

**Status**: already built and in active use.

### Wardline — the trust-topology enforcer

**Role**: declares and enforces trust topology at commit cadence.

Wardline understands "which code is allowed to do what." Modules declare their trust tier (`INTEGRAL`, `ASSURED`, `GUARDED`, `EXTERNAL_RAW`) and annotate functions with decorators that assert behavioural constraints (`@validates_shape`, `@integral_writer`, `@fail_closed`, `@handles_secrets`, and 38 others across 17 annotation groups). Wardline's scanner verifies that code satisfies what it claims, emits findings when it doesn't, and maintains a per-function fingerprint baseline so drift is visible.

**Authoritative for**: tier declarations, annotation semantics, trust-topology invariants, dataflow enforcement.

**Typical invocation**: `wardline scan` at commit cadence (pre-commit hook or CI); SARIF output uploaded to GitHub Security.

**Status**: already built and in active use.

### Shuttle — transactional change executor (proposed)

**Role**: executes an already-scoped change plan against the working tree with ordered edits, gated checks, rollback, and telemetry.

Shuttle is the Weft suite's change-execution layer. It receives a scoped change intent, binds it to concrete files or entities, orders the edits, applies them incrementally with pre- and post-change checks, rolls back on failure, and lints / commits / emits telemetry on success. It does **not** plan changes (Filigree tracks work), reason about correctness (Wardline and tests do), or understand code structure (Loomweave does).

**Authoritative for**: the transactional execution record of a code change.

**Typical invocation**: none yet; design not started.

**Status**: proposed. No design document. [weft.md](./weft.md) §7 describes the go/no-go test that gates new Weft products.

---

## How they interact

The suite is composed via two narrow protocols and a shared identity scheme.

### The fabric at a glance

```
                          ┌─────────────────┐
                          │   Filigree      │
                          │ issues,         │
                          │ findings,       │
                          │ observations    │
                          └────┬─────────▲──┘
                               │         │
                      findings │         │ read (triage state,
           (POST /api/v1/      │         │  cross-refs)
              scan-results)    │         │
                               ▼         │
   ┌──────────────┐     ┌──────────────┐
   │   Loomweave    ├────►│  scan import │
   │  catalog +   │     │  + observations│
   │  briefings   │◄────┤              │
   └──────▲───────┘     └──────────────┘
          │
          │ ingest (wardline.yaml,
          │  fingerprint.json,
          │  exceptions.json,
          │  REGISTRY)
          │
   ┌──────┴───────┐
   │   Wardline   │
   │  scanner +   │
   │  SARIF       │
   └──────────────┘
```

### Data flows

| Flow | From | To | Mechanism |
|---|---|---|---|
| Declared topology | Wardline manifest / fingerprint files | Loomweave catalog | File read at `loomweave analyze` |
| Annotation vocabulary | `wardline.core.registry.REGISTRY` | Loomweave's Python plugin | Direct import at plugin startup |
| Findings | Loomweave | Filigree | `POST /api/v1/scan-results` (Loomweave-native schema) |
| Findings (Wardline-sourced) *(v0.2)* | Wardline SARIF → Loomweave translator | Filigree | `POST /api/v1/scan-results` via `loomweave sarif import`; deferred in v0.1 per Loomweave ADR-015 — retires when Wardline emits natively to Filigree |
| Observations | Loomweave consult mode | Filigree | MCP tool call (or HTTP once the endpoint ships) |
| Entity state | Loomweave | Wardline (v0.2+) | Loomweave HTTP read API; Wardline currently re-scans |
| Issue cross-references | Filigree | Loomweave consult surface | Filigree read API |

### Identity and the shared vocabulary

The glue between tools is the **entity ID**. Loomweave owns the entity catalog and mints stable symbolic identifiers (`python:class:auth.tokens::TokenManager`). Filigree issues reference entities by Loomweave ID. Wardline findings carry qualnames that Loomweave reconciles to entity IDs at ingest. The suite has three concurrent identity schemes (Loomweave EntityId, Wardline qualname, Wardline exception-register location string) — Loomweave maintains the translation layer; neither sibling tool takes on that responsibility.

Findings are the other glue: every tool emits findings into Filigree's `POST /api/v1/scan-results` with a distinct `scan_source` (`loomweave`, `wardline`, and so on). Filigree preserves the `metadata` dict verbatim, so Loomweave's richer fields (`kind`, `confidence`, `related_entities`) and Wardline's SARIF property-bag extensions survive ingest under namespaced keys (`metadata.loomweave.*`, `metadata.wardline_properties.*`).

---

## Principles that shape the suite

Four commitments keep the Weft products from drifting into overlap (see [weft.md](./weft.md) for the suite's full doctrine, including the federation axiom and the composition law):

1. **Loomweave observes, Wardline enforces.** Loomweave detects that an annotation is present; Wardline determines whether the annotated code satisfies the semantic it declares. Loomweave never re-implements Wardline analyses; Wardline never re-implements Loomweave's graph.
2. **Findings are facts, not just errors.** A unified `Finding` record type carries defects, structural observations, classifications, metrics, and suggestions across all Weft products.
3. **Each tool is independently useful.** Loomweave works without Filigree (writes findings to local JSONL). Wardline works without Loomweave (has since day one). Filigree works without either.
4. **Local-first, single-binary, git-committable state.** No hosted service is required; `.weft/loomweave/`, `.weft/filigree/`, and Wardline's JSON state files are all meant to be committed and shared.

---

## Current state

| Tool | Built? | In use? | First customer |
|---|---|---|---|
| Filigree | Yes | Yes — active development | `filigree` itself; this project |
| Wardline | Yes | Yes — commit-cadence scanner | Wardline's own codebase |
| Loomweave | Partial — Sprint 1 walking skeleton tagged `v0.1-sprint-1`; v0.1 build in flight | Not yet — pre-v0.1 release | `elspeth` (~425k LOC Python) targeted for v0.1 validation |
| Shuttle | No — proposed; no design yet | Not yet | None — not yet scoped |

### What Loomweave v0.1 ships

A single-binary Rust core plus a Python language plugin. The core handles storage, LLM orchestration, clustering, and MCP read-only consult; the plugin handles Python parsing, import resolution, and entity extraction.

v0.1 is scoped as **minimal-core plus the Filigree registry handover**:

- Entity catalog + code graph + guidance sheets, SQLite-backed.
- Python-plugin parsing and entity extraction.
- Local `findings.jsonl` writer.
- MCP read-only consult surface.
- Filigree `registry_backend: loomweave` integration so Loomweave owns the file registry end-to-end. Filigree-side work lands alongside Loomweave's own release.

Deferred to v0.2 with written retirement conditions:

- **Wardline→Filigree SARIF bridge.** Wardline findings flow to Filigree only when Wardline ships its own native Filigree emitter (Loomweave ADR-015). Until then, the (Wardline, Filigree) pair composes outside Loomweave, via Wardline's existing SARIF-to-GitHub-Security path. `weft.md` §5 names this as a v0.1 asterisk.
- **Observation HTTP transport.** Loomweave emits observations via MCP tool calls in v0.1; a dedicated Filigree HTTP endpoint lands in v0.2.
- **Loomweave HTTP write API and summary cache beyond in-memory.** Read-only consult in v0.1; write surface deferred.

### What the suite needs from Filigree and Wardline for Loomweave to ship

Several changes land in the sibling tools as Loomweave-v0.1 prerequisites. All three products are maintained together, so these are within-scope work items rather than external dependencies:

- **Filigree (v0.1)**: a pluggable `registry_backend` (authored jointly with Loomweave ADR-014) so Loomweave can own the file registry; a published schema-compatibility contract (`NFR-COMPAT-01`).
- **Filigree (v0.2)**: an HTTP endpoint for observation creation.
- **Wardline (v0.1)**: a stable `REGISTRY_VERSION` that Loomweave's plugin pins against; a commitment to maintain legacy-decorator aliases.
- **Wardline (v0.2)**: a native emitter to Filigree so Loomweave's SARIF translator can be retired per ADR-015.

Loomweave's v0.1 design set spells these asks out in [system-design.md](../loomweave/1.0/system-design.md) and [detailed-design.md](../loomweave/1.0/detailed-design.md). Loomweave ships with degraded-mode fallbacks (`--no-filigree`, `--no-wardline`) so operators using only part of the suite still get a coherent product.

---

## Where to read next

| If you want to… | Read |
|---|---|
| Read Weft's founding doctrine — federation axiom, composition law, go/no-go test | [weft.md](./weft.md) |
| Enter the Loomweave v0.1 docset in reading order | [../loomweave/1.0/README.md](../loomweave/1.0/README.md) |
| Read Loomweave's requirements | [../loomweave/1.0/requirements.md](../loomweave/1.0/requirements.md) |
| Read Loomweave's system design | [../loomweave/1.0/system-design.md](../loomweave/1.0/system-design.md) |
| Read Loomweave's detailed design reference | [../loomweave/1.0/detailed-design.md](../loomweave/1.0/detailed-design.md) |
| Read accepted architecture decisions | [../loomweave/adr/README.md](../loomweave/adr/README.md) |
| Review the planning and review archive | [../implementation/README.md](../implementation/README.md) |
| Work with Filigree today | Check out the Filigree repository; start with its `CLAUDE.md` and `filigree --help`. |
| Work with Wardline today | Check out the Wardline repository; start with `docs/spec/`. |
