# PDR-0008: Expose briefing-blocked entities' SEI on the MCP read surface (reverse the "sei stays null" secret-handling posture)

- **Date:** 2026-06-29
- **Status:** accepted — **owner-ratified** (this reverses a deliberate secret-handling control; the code change is in-grant, the posture reversal was escalated and approved in-session by john@pgpl.net before implementation)
- **PRD:** none (reliability/correctness fix that surfaced from the warpline churn keying-gap diagnosis, PDR-0006 follow-up; not a fresh PRD-scoped bet)
- **Tracker:** clarion-4b3061b1ac (keying-gap issue, fixed by branch `fix/briefing-blocked-sei-federation-key`, closed on merge); clarion-obs-acffc4e8a1 (deep-pagination half, split out, still open); ADR-034 2026-06-29 amendment

## Context

The warpline churn surfaces (`entity_high_churn_list` / `entity_recent_change_list`,
lit up by PDR-0006) read churn `0` for every entity in a secret-bearing file (e.g.
all of lacuna's `tour/steps.py`), disclosed but not closed by the `churn_unresolved`
honesty block. Systematic-debugging traced the root cause to a place neither the
filed observation nor the first analysis predicted:

**loomweave's MCP read/resolve surface deliberately nulled the `sei` for
briefing-blocked (secret-bearing) entities** — even when an alive SEI binding
existed (`blocked_entity_stub` / `stack_entity_json` / `compact_blocked_node_json`
in loomweave-mcp). Warpline's self-heal (`reresolve-sei`) resolved the qualname
fine but received `sei: null`, so `entity_keys.sei` stayed NULL → the SEI churn
join missed → secret-bearing files read `0`. Live contrast on lacuna: `tour.steps._run`
resolved with `sei: null` + `briefing_blocked` *despite* an alive binding
(`loomweave:eid:a82891aadb36…`); `tests/` entities healed.

The null was an ADR-034 *gloss* layered onto the A3 projection
(clarion-719e7320f5) — A3 itself ("redact secret CONTENT, not entity IDENTITY")
restored `id`/`name`/`path`/`content_hash` and is **silent on the SEI**. So the
surface already exposed the locator + content hash (both more revealing than an
opaque SEI hash), yet withheld the one field every federation sibling keys on.

## Options

1. **Leave the redaction; accept the undercount** — secret-bearing files
   permanently read churn `0` (honestly disclosed via `churn_unresolved`). Rejected:
   a permanent correctness hole in a shipped surface, for no real secrecy gain.
2. **Bridge dialects loomweave-side** (emit a path-form locator) — rejected: the
   SEI binding *exists*; the gap is loomweave withholding it, not a dialect problem.
   Bridging would be fragile and wouldn't fix the SEI join.
3. **Expose the bound, content-free SEI on the blocked-entity read path** (chosen)
   — the SEI rides along via a new `blocked_sei` helper, *except* when the `id` is
   itself secret-like (then the durable key is withheld with its locator). Secret
   content (summary/source/docstring) stays withheld; the stricter HTTP
   `BRIEFING_BLOCKED` surface (ADR-034 §3) is unchanged.

## The call

**Option 3 — escalated and owner-ratified before implementation.** This is a
security-posture reversal, not a silent bug fix. The residual the prior posture
defended — a sibling *durably* binding a secret-bearing entity by a rename-surviving
key — was weighed against the cost and accepted: the SEI is a content-free hash
(ADR-038) strictly less revealing than the already-exposed locator + content hash;
REQ-C-04/ADR-038 already require every surface returning an `id` to carry its SEI;
and loomweave already emits that SEI *ephemerally* on the churn-query seam
(`sei_for_locator`, no briefing-block check) regardless — so the prior posture was
not even enforced end-to-end. The permanent churn-undercount cost outweighs the
narrow residual.

Implemented TDD-first on `fix/briefing-blocked-sei-federation-key` (`blocked_sei`
helper across all three blocked projections; red→green tests on each surface; the
4 `entity_resolve` integration tests flipped from "sei absent" to "sei rides
along"). CI floor green (the lone nextest failure is the pre-existing
wardline-sibling-drift oracle, clarion-72e1c1a07d). Live-proven on lacuna. Recorded
as the 2026-06-29 amendment to ADR-034.

## Reversal trigger

Reopen this decision if a concrete threat is identified where a sibling **durably
binding** a briefing-blocked entity by its content-free SEI leaks more than the
already-exposed locator + content hash — i.e. the rename-surviving cross-tool
handle to a secret location is judged to outweigh the churn-undercount cost it
removes. Metric anchor: if exposing the SEI is later shown to enable a sibling to
reconstruct withheld secret *content* (not just identity), revert immediately —
verified impossible at decision time (the SEI is a hash of identity, not content),
and already moot since the churn-query seam emits it regardless.
