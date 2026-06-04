# The Loom Suite — A Briefing

**Audience**: engineers, reviewers, or stakeholders new to the Loom suite
**Purpose**: explain what each tool does, how they fit together, and what state the suite is in today
**Reading time**: ~5 minutes

---

## The one-paragraph version

**Loom** is a suite for enterprise-grade code governance on small teams. Its v0.1 products — **Clarion**, **Filigree**, and **Wardline** — are three independent tools that enrich one another through narrow additive protocols. Each is fully authoritative in its domain and fully usable on its own. Clarion builds a trustworthy catalog of a codebase and answers structural questions. Filigree tracks the issues, findings, and observations that arise from examining that codebase. Wardline declares and enforces the trust topology that constrains how code is allowed to behave. Together they deliver rigor that normally requires enterprise-scale platform teams — without the operational weight, and without any shared runtime, store, or orchestrator. A fourth product, **Shuttle**, is proposed for transactional scoped change execution; see [loom.md](./loom.md) for the suite's founding doctrine, the enrichment-not-load-bearing principle, and the go/no-go test that governs future products.

> **Authoritative source.** The Loom federation axiom, roster, and composition law are now authoritative at the federation hub: [`~/loom/doctrine.md`](file:///home/john/loom/doctrine.md) (as of 2026-06-05). The hub's canonical roster is **5 realized members** — Clarion, Filigree, Wardline, **Legis**, and **Charter** — plus **Shuttle as a roadmap thought-bubble**; the three-member v0.1 framing in this briefing predates Legis and Charter and is kept as Clarion's local intro. This briefing remains Clarion's own introduction to the suite as Clarion sees it.

---

## The Loom products

### Clarion — the code-archaeology catalog

**Role**: indexes the source tree and answers structural questions.

Clarion ingests a codebase, extracts entities (functions, classes, modules, packages), clusters them into subsystems, and produces structured briefings that summarise each entity's purpose, maturity, and relationships. Consult-mode LLM agents query Clarion through MCP tools so they never need to spawn an explore-agent to answer "what are the entry points?" or "what calls this function?" — Clarion answered that during its batch analysis and caches the result.

**Authoritative for**: the entity catalog, the code graph, guidance sheets (institutional knowledge attached to entities), and structural / factual findings.

**Typical invocation**: `clarion analyze <project>` for batch indexing; `clarion serve` for MCP + HTTP consult.

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

Shuttle is the Loom suite's change-execution layer. It receives a scoped change intent, binds it to concrete files or entities, orders the edits, applies them incrementally with pre- and post-change checks, rolls back on failure, and lints / commits / emits telemetry on success. It does **not** plan changes (Filigree tracks work), reason about correctness (Wardline and tests do), or understand code structure (Clarion does).

**Authoritative for**: the transactional execution record of a code change.

**Typical invocation**: none yet; design not started.

**Status**: proposed. No design document. [loom.md](./loom.md) §7 describes the go/no-go test that gates new Loom products.

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
   │   Clarion    ├────►│  scan import │
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
| Declared topology | Wardline manifest / fingerprint files | Clarion catalog | File read at `clarion analyze` |
| Annotation vocabulary | `wardline.core.registry.REGISTRY` | Clarion's Python plugin | Direct import at plugin startup |
| Findings | Clarion | Filigree | `POST /api/v1/scan-results` (Clarion-native schema) |
| Findings (Wardline-sourced) *(v0.2)* | Wardline SARIF → Clarion translator | Filigree | `POST /api/v1/scan-results` via `clarion sarif import`; deferred in v0.1 per Clarion ADR-015 — retires when Wardline emits natively to Filigree |
| Observations | Clarion consult mode | Filigree | MCP tool call (or HTTP once the endpoint ships) |
| Entity state | Clarion | Wardline (v0.2+) | Clarion HTTP read API; Wardline currently re-scans |
| Issue cross-references | Filigree | Clarion consult surface | Filigree read API |

### Identity and the shared vocabulary

The glue between tools is the **entity ID**. Clarion owns the entity catalog and mints stable symbolic identifiers (`python:class:auth.tokens::TokenManager`). Filigree issues reference entities by Clarion ID. Wardline findings carry qualnames that Clarion reconciles to entity IDs at ingest. The suite has three concurrent identity schemes (Clarion EntityId, Wardline qualname, Wardline exception-register location string) — Clarion maintains the translation layer; neither sibling tool takes on that responsibility.

Findings are the other glue: every tool emits findings into Filigree's `POST /api/v1/scan-results` with a distinct `scan_source` (`clarion`, `wardline`, and so on). Filigree preserves the `metadata` dict verbatim, so Clarion's richer fields (`kind`, `confidence`, `related_entities`) and Wardline's SARIF property-bag extensions survive ingest under namespaced keys (`metadata.clarion.*`, `metadata.wardline_properties.*`).

---

## Principles that shape the suite

Four commitments keep the Loom products from drifting into overlap (see [loom.md](./loom.md) for the suite's full doctrine, including the federation axiom and the composition law):

1. **Clarion observes, Wardline enforces.** Clarion detects that an annotation is present; Wardline determines whether the annotated code satisfies the semantic it declares. Clarion never re-implements Wardline analyses; Wardline never re-implements Clarion's graph.
2. **Findings are facts, not just errors.** A unified `Finding` record type carries defects, structural observations, classifications, metrics, and suggestions across all Loom products.
3. **Each tool is independently useful.** Clarion works without Filigree (writes findings to local JSONL). Wardline works without Clarion (has since day one). Filigree works without either.
4. **Local-first, single-binary, git-committable state.** No hosted service is required; `.clarion/`, `.filigree/`, and Wardline's JSON state files are all meant to be committed and shared.

---

## Current state

| Tool | Built? | In use? | First customer |
|---|---|---|---|
| Filigree | Yes | Yes — active development | `filigree` itself; this project |
| Wardline | Yes | Yes — commit-cadence scanner | Wardline's own codebase |
| Clarion | Partial — Sprint 1 walking skeleton tagged `v0.1-sprint-1`; v0.1 build in flight | Not yet — pre-v0.1 release | `elspeth` (~425k LOC Python) targeted for v0.1 validation |
| Shuttle | No — proposed; no design yet | Not yet | None — not yet scoped |

### What Clarion v0.1 ships

A single-binary Rust core plus a Python language plugin. The core handles storage, LLM orchestration, clustering, and MCP read-only consult; the plugin handles Python parsing, import resolution, and entity extraction.

v0.1 is scoped as **minimal-core plus the Filigree registry handover**:

- Entity catalog + code graph + guidance sheets, SQLite-backed.
- Python-plugin parsing and entity extraction.
- Local `findings.jsonl` writer.
- MCP read-only consult surface.
- Filigree `registry_backend: clarion` integration so Clarion owns the file registry end-to-end. Filigree-side work lands alongside Clarion's own release.

Deferred to v0.2 with written retirement conditions:

- **Wardline→Filigree SARIF bridge.** Wardline findings flow to Filigree only when Wardline ships its own native Filigree emitter (Clarion ADR-015). Until then, the (Wardline, Filigree) pair composes outside Clarion, via Wardline's existing SARIF-to-GitHub-Security path. `loom.md` §5 names this as a v0.1 asterisk.
- **Observation HTTP transport.** Clarion emits observations via MCP tool calls in v0.1; a dedicated Filigree HTTP endpoint lands in v0.2.
- **Clarion HTTP write API and summary cache beyond in-memory.** Read-only consult in v0.1; write surface deferred.

### What the suite needs from Filigree and Wardline for Clarion to ship

Several changes land in the sibling tools as Clarion-v0.1 prerequisites. All three products are maintained together, so these are within-scope work items rather than external dependencies:

- **Filigree (v0.1)**: a pluggable `registry_backend` (authored jointly with Clarion ADR-014) so Clarion can own the file registry; a published schema-compatibility contract (`NFR-COMPAT-01`).
- **Filigree (v0.2)**: an HTTP endpoint for observation creation.
- **Wardline (v0.1)**: a stable `REGISTRY_VERSION` that Clarion's plugin pins against; a commitment to maintain legacy-decorator aliases.
- **Wardline (v0.2)**: a native emitter to Filigree so Clarion's SARIF translator can be retired per ADR-015.

Clarion's v0.1 design set spells these asks out in [system-design.md](../clarion/v0.1/system-design.md) and [detailed-design.md](../clarion/v0.1/detailed-design.md). Clarion ships with degraded-mode fallbacks (`--no-filigree`, `--no-wardline`) so operators using only part of the suite still get a coherent product.

---

## Where to read next

| If you want to… | Read |
|---|---|
| Read Loom's founding doctrine — federation axiom, composition law, go/no-go test | [loom.md](./loom.md) |
| Enter the Clarion v0.1 docset in reading order | [../clarion/v0.1/README.md](../clarion/v0.1/README.md) |
| Read Clarion's requirements | [../clarion/v0.1/requirements.md](../clarion/v0.1/requirements.md) |
| Read Clarion's system design | [../clarion/v0.1/system-design.md](../clarion/v0.1/system-design.md) |
| Read Clarion's detailed design reference | [../clarion/v0.1/detailed-design.md](../clarion/v0.1/detailed-design.md) |
| Read accepted architecture decisions | [../clarion/adr/README.md](../clarion/adr/README.md) |
| Review the planning and review archive | [../implementation/README.md](../implementation/README.md) |
| Work with Filigree today | Check out the Filigree repository; start with its `CLAUDE.md` and `filigree --help`. |
| Work with Wardline today | Check out the Wardline repository; start with `docs/spec/`. |
