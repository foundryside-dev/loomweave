# ADR-016: Observation Transport — MCP-Spawn (v0.1), Filigree HTTP Endpoint (v0.2)

**Status**: Accepted
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: Filigree's observation API is MCP-only today; v0.1 scope commitment deferred the HTTP endpoint to v0.2

## Summary

Clarion emits observations (LLM-proposed guidance, unknown-vocabulary candidates, `knowledge_basis` signals) to Filigree. In v0.1, Clarion spawns a `filigree mcp` subprocess and uses Filigree's existing MCP `create_observation` tool as the transport. In v0.2, Filigree adds `POST /api/v1/observations`; Clarion's observation emit path migrates to HTTP and the subprocess-spawn path retires. The Q1 scope commitment (`v0.1-scope-commitments.md`) explicitly defers the HTTP endpoint to v0.2 — not because it is technically difficult, but because Filigree-side work is already loaded with `registry_backend` surgery (ADR-014) on the v0.1 timeline. MCP-spawn works against the existing Filigree MCP server without Filigree-side change.

## Context

Filigree's observation API exists as an MCP tool (`mcp_tools/observations.py` — `create_observation`, `list_observations`, `promote_observation`). There is no HTTP endpoint. The original Clarion design (system-design §9) framed `POST /api/v1/observations` as the preferred transport with MCP-spawn as a fallback when Filigree hadn't shipped the endpoint yet. The 2026-04-17 panel's scope-commitment review re-litigated which Filigree-side work belongs in v0.1 and which belongs in v0.2.

The Q1 decision (`v0.1-scope-commitments.md`) committed:

> **v0.1 scope**: Minimal-core + `registry_backend`. Deferred to v0.2: Wardline→Filigree SARIF bridge, **observation HTTP transport**, summary cache beyond in-memory, HTTP write API.

That commitment reverses the original "preferred / fallback" framing. In v0.1, the HTTP endpoint does not exist; the capability probe has nothing to detect; MCP-spawn is the v0.1 path, not a fallback. This ADR records that commitment and names the v0.2 retirement trigger.

Clarion is a Rust binary. Spawning `filigree mcp` as a subprocess and speaking MCP over stdio is a known shape (it's exactly the plugin transport, ADR-002). The engineering cost is real but bounded: Rust MCP client crate + process supervision + stdio framing. The alternative — adding `POST /api/v1/observations` to Filigree — is also real engineering (new Flask/FastAPI route, MCP-to-HTTP parity, schema validation, auth plumbing) and competes with `registry_backend` work on the Filigree v0.1 timeline.

## Decision

### v0.1 transport — `filigree mcp` subprocess

Clarion's `clarion analyze` and `clarion serve` both spawn a `filigree mcp` subprocess and use it for observation emission.

- **Lifecycle**: one subprocess per `clarion analyze` invocation, spawned at Phase 0 alongside the capability probe. Kept alive for the duration of the run; terminated at the final phase boundary. `clarion serve` spawns its own subprocess at startup, kept alive for the lifetime of the serve process.
- **Transport**: stdio JSON-RPC 2.0 with Content-Length framing (same framing as plugin transport, ADR-002). Clarion reuses the framing implementation.
- **MCP tools called**: `create_observation(entity_id, text, source, file_path?, line?)` and `promote_observation(obs_id, issue_template?)` (the latter only from `clarion serve`'s MCP tool surface).
- **Supervision**: process crashes emit `CLA-INFRA-FILIGREE-MCP-CRASHED`; crash-loop circuit breaker (>3 crashes in 60s, same threshold as plugins) disables observation emission for the run. Observations emitted after disable are written to `runs/<run_id>/deferred_observations.jsonl` for manual replay.
- **Filigree binary discovery**: `clarion.yaml:integrations.filigree.binary_path` (default: `filigree` on `PATH`). Not found → `CLA-INFRA-FILIGREE-BINARY-MISSING`; falls back to `--no-filigree` mode (observations written only to `deferred_observations.jsonl`).

### v0.2 retirement — `POST /api/v1/observations`

When Filigree ships the HTTP endpoint:

- Capability probe (system-design §11 step 3) detects it via `HEAD /api/v1/observations` returning 200.
- Clarion's observation emit path switches to HTTP. The `filigree mcp` subprocess is no longer spawned for observation emission.
- The deferred-observations replay mechanism remains for `--no-filigree` runs.
- `clarion serve`'s MCP tools (`emit_observation`, `promote_observation`) continue to exist on Clarion's side — they are consult-agent-facing, not Filigree-facing. They translate internally to the new HTTP emit path.
- The `CLA-INFRA-FILIGREE-OBS-VIA-MCP` finding (previously listed in the compat-report fallback table) is removed from v0.2 — MCP-via-subprocess is no longer the expected v0.1 degradation.

### Retirement trigger

Filigree publishes `POST /api/v1/observations` with a schema parallel to the MCP `create_observation` tool. Clarion's capability probe `HEAD /api/v1/observations` returning 200 is the specific switch-over signal. Filigree's CHANGELOG is the authoritative announcement; Clarion's compat-report signals presence.

### Consult-tool emit path (unchanged by transport flip)

Clarion's own MCP tool `emit_observation(id, text)` (exposed on `clarion serve`'s MCP surface for consult-mode agents) is transport-independent. In v0.1 it writes to the `filigree mcp` subprocess; in v0.2 it writes via HTTP. The MCP tool's contract to consult agents doesn't change.

## Alternatives Considered

### Alternative 1: Filigree HTTP endpoint in v0.1

Add `POST /api/v1/observations` to Filigree's v0.1 release; Clarion emits via HTTP from day one.

**Pros**: simpler Clarion-side — one transport instead of two; no subprocess management; no Filigree binary path dependency. Matches the original design's "preferred" framing.

**Cons**: Filigree-side engineering competes with `registry_backend` schema surgery (ADR-014) on the v0.1 timeline. The HTTP endpoint is not technically difficult but is not free — route handler, schema validation, MCP-to-HTTP tool parity, auth, tests. Q1 scope commitment explicitly deferred this to v0.2 so that v0.1 Filigree-side work could focus on the one surgery (`registry_backend`) that cannot be worked around.

**Why rejected**: Q1 commitment locks the v0.1 scope. The MCP-spawn workaround is functional and bounded; Clarion ships on time, Filigree-side work focuses on `registry_backend`, and v0.2 adds the HTTP endpoint cleanly.

### Alternative 2: TCP MCP transport instead of subprocess spawn

Clarion connects to a running `filigree serve` over TCP (Filigree's MCP-over-TCP mode) rather than spawning a subprocess.

**Pros**: no per-run subprocess spawn cost; works with teams running a central Filigree instance; no `filigree` binary path dependency on each dev host.

**Cons**: Filigree's MCP-over-TCP is less mature than stdio; adds TLS/auth/retry plumbing to the transport layer that stdio avoids. Clarion's v0.1 is explicitly local-first (CON-LOCAL-01) — spawning a subprocess matches that posture better than dialing a network service. The scope-commitment memo's "Q1 minimal-core" posture doesn't include managing a persistent Filigree daemon.

**Why rejected**: TCP transport's surface area doesn't pay off at v0.1 scale. Subprocess spawn is the lower-cost option consistent with the local-first posture.

### Alternative 3: Coerce observations into the finding pipeline (ADR-004 path)

Emit observations as a class of finding (`kind: suggestion`, `scan_source: clarion-obs`) through `POST /api/v1/scan-results`. No separate transport.

**Pros**: reuses the already-committed finding transport (ADR-004); zero new transport surface.

**Cons**: observations have a different lifecycle than findings. Filigree observations auto-expire after 14 days (`mcp_tools/observations.py` lifetime policy); findings persist until explicitly closed. Forcing observations through the finding pipeline misrepresents their semantic: operators would see "observations" under the findings UI with no clean signal that they're ephemeral. Filigree's `promote_observation` tool (promotes an observation to an issue) has no finding-path analogue; that workflow breaks.

**Why rejected**: observations and findings are semantically distinct; the transport shouldn't collapse the distinction.

### Alternative 4: Defer observations entirely to v0.2

v0.1 does not emit observations. Observation generators (LLM-proposed guidance, vocabulary candidates) queue signals locally; v0.2 adds both the HTTP endpoint and the emit path.

**Pros**: zero transport code in v0.1; no subprocess, no Filigree-binary dependency.

**Cons**: observations are a load-bearing feedback channel in v0.1. `propose_guidance` (ADR-009) produces observations, not sheets — the guidance-promotion gate *requires* observations to exist for an operator to review. `CLA-FACT-VOCABULARY-CANDIDATE` and similar signals are observation-shaped. Deferring the transport means deferring the feature.

**Why rejected**: the v0.1 feature set includes observation *emission*; deferring *transport* would defer the feature by proxy.

## Consequences

### Positive

- v0.1 ships without requiring Filigree-side HTTP endpoint work. Filigree's v0.1 engineering focus is `registry_backend` (ADR-014); observations ride on existing MCP infrastructure.
- The spawn approach works today — `filigree mcp` + `create_observation` already exist. Clarion's side is the new code; Filigree's side is zero change.
- Retirement path is explicit: v0.2 HTTP endpoint lands, capability probe detects, Clarion's emit path switches. No ambiguity about when or how.
- Consult-tool contract (`emit_observation` MCP tool on `clarion serve`) is transport-independent. v0.1 → v0.2 migration is invisible to consult agents.

### Negative

- Subprocess management in Clarion — spawn, stdio I/O, process supervision, crash-loop handling, termination at shutdown. All real code paths that ADR-002's plugin supervision already has; Clarion reuses that implementation but it's still surface to test.
- `filigree` binary must be on `PATH` (or explicitly configured) for observation emission. Fresh-install developer workstations need the Filigree CLI installed for Clarion to emit observations; otherwise fallback to `deferred_observations.jsonl`. Mitigation: the SUITE-COMPAT-REPORT finding (system-design §11) names the binary-missing case explicitly.
- One extra subprocess per `clarion analyze` run (+ one persistent for `clarion serve`). A few MB RSS each. Tolerable at v0.1 scale; not free.
- `deferred_observations.jsonl` replay is an explicit operator step (`clarion observations replay`); observations generated during a degraded run are not self-healing. Mitigation: the deferred-observations file is a first-class part of the `runs/<run_id>/` structure, not a hidden error log.

### Neutral

- The stale system-design §9 and §11 passages framing HTTP as "preferred" and MCP-spawn as "fallback" are updated alongside this ADR's acceptance — the capability-probe fallback table row for "`/api/v1/observations` absent" becomes a v0.2 feature-presence check, not a degradation indicator.
- The `CLA-INFRA-FILIGREE-OBS-VIA-MCP` finding listed in the original design is re-scoped: it was framed as a degradation marker; under this ADR it marks the *expected* v0.1 state and retires alongside the transport in v0.2.

## Related Decisions

- [ADR-002](./ADR-002-plugin-transport-json-rpc.md) — the subprocess + stdio + Content-Length framing shape is shared with plugin transport. Clarion reuses ADR-002's implementation for the `filigree mcp` subprocess.
- [ADR-014](./ADR-014-filigree-registry-backend.md) — the scope-commitment counterweight. `registry_backend` is the one Filigree-side surgery v0.1 commits to; deferring the observation HTTP endpoint is what makes that focus possible.
- ADR-009 (pending) — the structured-briefing / propose-guidance decision produces observations via the transport defined here.

## References

- [Clarion v0.1 scope commitments — Q1](../../implementation/v0.1-scope-plans/v0.1-scope-commitments.md) — observation HTTP transport explicitly deferred to v0.2.
- [Clarion v0.1 system design §9 (Observation transport)](../v0.1/system-design.md) (lines 916-921) — the passage reversed by this ADR; updated in the same commit.
- [Clarion v0.1 system design §11 (Capability negotiation)](../v0.1/system-design.md) (lines 1122-1141) — observation HTTP presence moves from v0.1 fallback trigger to v0.2 feature-flag detection.
- [Clarion v0.1 detailed design §9.1 Filigree prerequisites](../v0.1/detailed-design.md) (lines 1333-1351) — `POST /api/v1/observations` moved from "Required for v0.1 ship" to "Nice-to-have (v0.2+)".
