# ADR-015: Wardline→Filigree Emission Ownership — Clarion-Side Translator (v0.1), Native Wardline Emitter (v0.2)

**Status**: Accepted
**Date**: 2026-04-18
**Deciders**: qacona@gmail.com
**Context**: Wardline has no HTTP client today; getting its findings into Filigree needs an owner for v0.1 and a retirement plan for v0.2

## Summary

In v0.1, Wardline's findings reach Filigree via Clarion's `clarion sarif import <sarif_file> --scan-source wardline` CLI — a translator that reads Wardline's on-disk SARIF output, translates to Filigree-native intake (ADR-004), and POSTs to `/api/v1/scan-results`. In v0.2, Wardline gains a native `POST /api/v1/scan-results` emitter, which eliminates the Clarion-in-the-middle path for the (Wardline, Filigree) pair. The translator itself stays forever — it serves Semgrep, CodeQL, Trivy, and every other SARIF-emitting tool — but the translator's role as "v0.1 Wardline bridge" retires when the native emitter lands.

This arrangement is named as an explicit **pipeline-coupling asterisk** in `loom.md` §5 (asterisk 1). It violates pairwise composability between Wardline and Filigree until v0.2 and ships that way only with a written retirement condition. **Revision trigger**: if the Block C2 Wardline-native-emitter spike shows the native emitter is ≤1 day of work, this ADR is revised in place to promote the native emitter into v0.1 and retire the loom.md §5 asterisk.

## Context

Wardline today has no HTTP client (`reviews/pre-restructure/integration-recon.md:339`). `wardline/pyproject.toml:22` declares `dependencies = []` and the CI pipeline uploads SARIF to GitHub Security, not Filigree. For Wardline's findings to reach Filigree at all, something has to read Wardline's SARIF output and POST it.

The `loom.md` §5 failure test names pipeline coupling as a federation violation: "if a pair of sibling products (X, Z) cannot exchange data except through a third sibling (Y)." The (Wardline, Filigree) pair cannot compose directly in v0.1 — they compose only when Clarion is present and someone runs `clarion sarif import`. That is the triangle the panel flagged in its doctrine synthesis (`11-doctrine-panel-synthesis.md` paragraph on the SARIF triangle) and the scope-commitments memo confirmed as an open consequence (`v0.1-scope-commitments.md:72`).

Three decisions exist inside the emission question:

1. **Who owns the v0.1 path?** Clarion (Option A) or Wardline (Option B)?
2. **When does Wardline gain a native path?** v0.2 default, or a v0.1 stretch-goal contingent on a cheap implementation?
3. **What happens to the Clarion translator when Wardline gains native emission?** Does it retire entirely, or stay for other SARIF emitters?

The default committed answers in `../../implementation/v0.1-scope-plans/v0.1-scope-commitments.md:190` are: Option A for v0.1, Option B for v0.2 (default deferral, optional promotion pending spike), translator stays permanently for non-Wardline SARIF sources.

This ADR formalises those answers and names the revision trigger.

## Decision

### v0.1 position — Clarion-side SARIF translator

Clarion ships a CLI at `clarion sarif import <sarif_file> [--scan-source <name>]`. The v0.1 Wardline path is `clarion sarif import wardline.sarif.baseline.json --scan-source wardline`. The translator:

- Reads Wardline's SARIF output from disk.
- Translates to Filigree-native `POST /api/v1/scan-results` format (ADR-004).
- POSTs with `scan_source="wardline"` and `metadata.wardline_properties.*` preserving SARIF property-bag extension keys (ADR-019).
- Applies the severity mapping and rule-ID round-trip rules from ADR-017.

The translator is a general-purpose feature, not a Wardline-specific bridge. `--scan-source` defaults to the SARIF driver name, lowercased — `semgrep`, `codeql`, `trivy`, and any future SARIF emitter use the same translator with different `scan_source` values.

Operators run the translator as part of their CI pipeline (Wardline → SARIF on disk → `clarion sarif import` → Filigree) or on-demand after local Wardline runs. Wardline findings reach Filigree *only* when someone runs the translator — this is the "pipeline coupling" shape.

### v0.2 retirement — native Wardline emitter

Wardline gains an HTTP client (`httpx` or `requests`, to be decided Wardline-side) and emits findings directly to `POST /api/v1/scan-results` from its scanner path. When this lands:

- The `loom.md` §5 asterisk 1 retires. The briefing's "Wardline-sourced findings" data-flow row changes from v0.2-deferred to current.
- The translator's role as "v0.1 Wardline bridge" retires. The translator itself remains for non-Wardline SARIF sources; Wardline no longer flows through it.
- `--scan-source wardline` remains usable for historical SARIF baselines (e.g., re-ingesting an old Wardline scan) but stops being the production path.

Wardline-side commitments for v0.2:

- Add `httpx` (or equivalent) to `wardline/pyproject.toml` dependencies. This changes the product's "zero dependencies" posture deliberately, with the retirement of the federation asterisk as the justification.
- Write scan-result emission to a configurable Filigree endpoint; retry / auth plumbing owned Wardline-side.
- SARIF output remains (GitHub Security upload path unchanged); native Filigree emit is additive, not a replacement.

### Revision trigger — Block C2 spike

The default commitment assumes Wardline's native emitter is significant enough to defer to v0.2. The Block C2 spike (one day, optional) investigates whether adding `httpx` + a single emit call is ≤1 day of work end-to-end. If the spike shows it is:

- This ADR is revised in place (new dated revision, per `system-design.md:1206` writing-cadence rule). Position flips to Option B in v0.1.
- The `loom.md` §5 asterisk 1 is deleted.
- The briefing's "Wardline-sourced findings" data-flow row updates to current.
- The translator's framing loses its "v0.1 Wardline bridge" role from day one; it ships as a general-purpose SARIF ingest path.

If the spike is not run, or shows the refactor is >1 day, this ADR stands as-is and the retirement condition lives in `loom.md` §5.

### Spike result (research-level, 2026-04-18)

A research pass (code inspection only, no implementation) against `/home/john/wardline/` produced these findings:

- **Dependency posture**: `pyproject.toml:22` has `dependencies = []`. Optional `scanner` deps already include `pyyaml`, `jsonschema`, `click`; optional `bar` deps already include `anthropic>=0.50.0`. Adding `httpx` as an optional `scanner` dep (or a new `filigree` optional) preserves the `dependencies = []` posture. The product is not dependency-averse in principle; it is dependency-disciplined in the optional-extras pattern.
- **Serialisation surface**: `scanner/sarif.py` already has `SarifReport.to_dict()`, `SarifReport.to_json_string()`, `SarifReport.to_json(path)`. Iterating `SarifReport.findings` is the natural hook for Filigree emission. No refactor of the scanner's internal types required.
- **Scan CLI integration**: `cli/scan.py:1144` constructs `SarifReport` with all the data a Filigree emission would need. Adding optional `--filigree-url` / `--filigree-token` CLI options (or reading them from a `wardline.yaml:filigree` config block) is a mechanical addition.
- **HTTP-client code**: no existing HTTP client in the scanner path (`grep` returns only the string `requests` appearing in decorator-resolution logic, unrelated). Net-new module required.
- **Effort estimate**: `filigree_emitter.py` ~200 LoC (severity/rule-ID mapping, POST with basic retry, auth header plumbing); `scan.py` integration ~50 LoC; config-schema additions ~30 LoC; tests ~100 LoC against a mock Filigree server. Total ~400 LoC + documentation. Realistic time: **~1 day of focused work** for a maintainer with the codebase in their head.

**Verdict (research-level)**: borderline at the promotion threshold (<1 day). A determined half-day is plausible for the minimum-viable emitter; an unhurried day is more realistic; an integration-test-thorough pass could go to 1.5 days. This is the kind of boundary where "cheap enough" is a judgement call, not a measurement.

**Decision (2026-04-18)**: ADR-015 stands as-is — **v0.1 = Clarion SARIF translator, v0.2 = Wardline native emitter**. Rationale: the research-level estimate is bounded but the v0.1 sprint is already loaded with 10 ADRs + auth flip + secret scanner implementation; adding 1 day of Wardline-side refactor plus its own cross-tool integration tests is a scope delta the user may choose to take on, but defaulting to the v0.2 commitment preserves the documented scope-commitment shape. The spike result is recorded here so a later decision to flip can cite concrete evidence without re-running the research.

Operators or maintainers reviewing this decision later: if the Wardline-native emitter is implemented as part of Clarion v0.1 work, revise this ADR with a dated "Revision 2" section, delete the `loom.md` §5 asterisk 1, and update the briefing's "Wardline-sourced findings" data-flow row.

## Alternatives Considered

### Alternative 1: Make SARIF the direct Clarion/Wardline → Filigree contract (Filigree grows SARIF ingest)

Filigree adds a `POST /api/v1/sarif` endpoint. Every emitting tool POSTs SARIF directly; no Clarion translator needed.

**Pros**: aligned with the broader SARIF tool ecosystem; eliminates translator ownership questions entirely.

**Cons**: Filigree's production intake is deliberately Filigree-native flat JSON (ADR-004). Growing a second ingest path doubles the schema surface Filigree must validate. Does not actually collapse the triangle — Wardline still has no HTTP client, so something still has to ship the file. Relocates Clarion's translator to Filigree's side, which makes Filigree the new centralisation site for SARIF interpretation.

**Why rejected**: ADR-004 already decided Filigree-native is the canonical Clarion→Filigree contract; SARIF is a translator input/output path. Reopening that at the Wardline boundary re-introduces the "invent a suite schema" option ADR-004 rejected.

### Alternative 2: Wardline native emitter in v0.1 (default-position promotion without spike)

Commit Option B for v0.1 unconditionally. Wardline adds an HTTP client in the v0.1 release cycle.

**Pros**: collapses the (Wardline, Filigree) triangle immediately; `loom.md` §5 asterisk 1 never exists.

**Cons**: Wardline's `dependencies = []` posture is deliberate, and changing it needs consideration that Wardline's own design owes rather than a Clarion ADR imposing it. The refactor adds retry/auth/error-handling plumbing to Wardline's scanner path, which currently ends at SARIF-on-disk. For a v0.1 timeline that already carries 10 P0 ADRs, a surprise day-2+ of Wardline-side plumbing work is real timeline risk.

**Why rejected as default**: the scope-commitments memo explicitly defers this to v0.2 unless the C2 spike shows it is cheap. The trigger condition preserves the upside (if it *is* cheap, v0.1 collapses the triangle) without committing to it sight-unseen.

### Alternative 3: Permanent Clarion translator ownership of Wardline findings

Option A for v0.1 and forever. Wardline never grows an HTTP client; Clarion always owns the Wardline→Filigree path.

**Pros**: no Wardline changes ever; Clarion's translator is a single owner.

**Cons**: the pipeline-coupling asterisk never retires. `(Wardline, Filigree)` can never compose without Clarion running, which permanently violates `loom.md` §5 for that pair. A three-product suite where one pair cannot compose pairwise without a third is exactly the shape Loom's failure test is supposed to prevent.

**Why rejected**: an asterisk without a retirement condition is not an asterisk — it is the stealth-monolith pattern. `loom.md` §5 itself says so ("Asterisks are acceptable only with a written retirement condition").

### Alternative 4: Custom streaming format — Wardline emits JSON Lines to stdout; Clarion tails

Wardline writes findings to stdout as JSON Lines; Clarion tails the process and POSTs. No SARIF, no file on disk.

**Pros**: avoids a disk round-trip; avoids SARIF format overhead.

**Cons**: invents a format that is neither SARIF (no future-proof ecosystem benefit) nor Filigree-native (still needs translation). Adds a Wardline-Clarion-specific format surface. Wardline's existing SARIF-on-disk output is useful independently (GitHub Security upload); a stdout-only path regresses that utility.

**Why rejected**: gains nothing over SARIF-on-disk + translator; adds format-maintenance cost.

### Alternative 5: Shared internal library consumed by both Wardline and Clarion

A `loom-findings` package that both products import; the library owns emission to Filigree.

**Pros**: no translator anywhere; findings always reach Filigree regardless of which tool produces them.

**Cons**: violates `loom.md` §6 ("no shared runtime, no shared configuration layer"). A shared library that both products import is a dependency-level stealth monolith — removing the library breaks both products simultaneously. Wardline's `dependencies = []` posture collapses the moment it imports `loom-findings`.

**Why rejected**: categorical federation violation.

## Consequences

### Positive

- Clarion ships v0.1 without requiring a Wardline-side refactor on Clarion's timeline. The translator exists today; the Wardline SARIF output exists today; the path works.
- The translator architecture is general, not Wardline-specific. Semgrep, CodeQL, Trivy, and future SARIF tools all use the same `clarion sarif import --scan-source <name>` pattern.
- The `loom.md` §5 asterisk 1 has a named retirement condition (native Wardline emitter in v0.2). The asterisk lives in the doctrine doc, not only in Clarion's design, so it is visible suite-wide.
- The revision trigger (Block C2 spike) is cheap to evaluate — one day of investigation, with a clear go/no-go threshold. If the native emitter is easy, v0.1 gets the triangle-collapse benefit without the timeline cost of committing to it first.

### Negative

- In v0.1, Wardline findings reach Filigree only when someone runs `clarion sarif import`. Operators who expect Wardline→Filigree to be automatic (CI pipeline, scheduled scan) must add the translator invocation explicitly. The failure mode — Wardline findings silently absent from Filigree — is plausible and needs documentation. Mitigation: the SUITE-COMPAT-REPORT finding (system-design §11) names whether `clarion sarif import` has run recently against each known SARIF source.
- The pipeline-coupling asterisk ships with v0.1. For a product whose marketing register includes "federation", this is a visible gap. Mitigation: it is named as an asterisk, not hidden — `loom.md` §5 carries the retirement condition, and the briefing's data-flow row is labelled v0.2.
- Wardline-side v0.2 work (HTTP client refactor) is within-scope because the user owns Wardline, but it is real engineering time. Mitigation: the C2 spike is the mechanism that clarifies the scope before committing v0.2.

### Neutral

- SARIF property-bag preservation (ADR-019) handles the 44 `wardline.*` extension keys; this ADR does not duplicate that decision.
- Severity mapping + rule-ID round-trip (ADR-017) handles the translation semantics; this ADR does not duplicate that decision either.
- The translator's long-term residency is unambiguous: it stays regardless of what Wardline does, because its audience extends beyond Wardline.

## Related Decisions

- [ADR-004](./ADR-004-finding-exchange-format.md) — Filigree-native intake is the target schema the translator maps to. SARIF is the input; Filigree-native is the output.
- [ADR-017](./ADR-017-severity-and-dedup.md) — severity mapping (SARIF `error`/`warning`/`note` → Clarion `ERROR`/`WARN`/`INFO` → Filigree `high`/`medium`/`info`) and rule-ID round-trip rules apply to this translator.
- ADR-019 (pending) — SARIF property-bag preservation (`metadata.<driver>_properties.*` pass-through) applies here. Wardline's 44 extension keys land under `metadata.wardline_properties.*`.
- [ADR-018](./ADR-018-identity-reconciliation.md) — SARIF ingest is translation entry point 3 (via `location.logicalLocations` or `partialFingerprints`). The identity-reconciliation rules named there apply to this translator's `entity_id` resolution.

## References

- [Loom doctrine §5 (v0.1 asterisks, asterisk 1)](../../suite/loom.md) — pipeline-coupling asterisk for (Wardline, Filigree) with this ADR as the retirement condition.
- [Clarion v0.1 detailed design §9 (Wardline prerequisites) item 3](../v0.1/detailed-design.md) (line 1361) — Option A / Option B enumeration.
- [Clarion v0.1 scope commitments](../../implementation/v0.1-scope-plans/v0.1-scope-commitments.md) (lines 72, 190, 202) — default deferral; spike promotion condition.
- [Clarion v0.1 integration reconnaissance §4.3](../../implementation/v0.1-reviews/pre-restructure/integration-recon.md) (line 339) — "Wardline has zero HTTP client code"; empirical basis for the v0.2 deferral default.
- [Panel doctrine synthesis](../../implementation/v0.1-reviews/panel-2026-04-17/11-doctrine-panel-synthesis.md) — SARIF triangle framing; asterisk requirement.
