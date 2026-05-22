# Clarion v1.0 — Requirements

**Status**: Baselined for v1.0 release (carried forward from v0.1 baseline 2026-04-17 with the Sprint 3 federation-hardening additions)
**Baseline**: 2026-04-17 · **Last updated**: 2026-05-19
**Primary author**: qacona@gmail.com (with Claude)
**First customer target**: `/home/john/elspeth` (~425k LOC Python)
**Companion documents**:
- [system-design.md](./system-design.md) — system design (the *how*, mid-level)
- [detailed-design.md](./detailed-design.md) — detailed design reference (implementation-level)
- [../adr/README.md](../adr/README.md) — Accepted ADRs
- [../../implementation/v0.1-reviews/pre-restructure/design-review.md](../../implementation/v0.1-reviews/pre-restructure/design-review.md) — archived design review that shaped revs 2-4
- [../../implementation/v0.1-reviews/pre-restructure/integration-recon.md](../../implementation/v0.1-reviews/pre-restructure/integration-recon.md) — archived reality check against Filigree / Wardline

---

## Preamble

### What this document is

This is the requirements specification for Clarion v1.0 — the *what*: capabilities, constraints, quality attributes, and explicit non-goals. The *how* (architecture, mechanisms, diagrams) lives in the system-design; the *implementation detail* (SQL schemas, Rust crate selection, rule-ID catalogues) lives in the detailed-design.

### How to read this

- Each requirement has a stable ID (`REQ-*`, `NFR-*`, `CON-*`, `NG-*`), a plain-English statement, a rationale, a verification method, and a **See** line pointing to the system-design section that addresses it.
- Requirement IDs are load-bearing: filigree issues cite them by ID in descriptions and commit messages; code review references them when discussing implementation. IDs are stable across rev bumps unless a requirement is fully retired (in which case the ID is never reused).
- "Clarion" throughout means Clarion v1.0 specifically. Where v2.0+ behaviour is deliberately different, it's named as a non-goal (`NG-*`) in the Non-Goals section of this document.

### Relationship to Loom

Clarion is one product in the **Loom** suite (see [../../suite/loom.md](../../suite/loom.md) for the family's founding doctrine). These requirements respect the Loom federation axiom: Clarion must be useful standalone, must compose pairwise with each sibling product, and must never become load-bearing for another product's semantics. That posture is formalised in `CON-LOOM-01`.

### Design principles (framing, not numbered requirements)

The five design principles from the detailed-design frame the specific requirements that follow. When a requirement could be interpreted more than one way, these principles break ties:

1. **Enterprise at lack of scale.** Bring governance / trust-topology / audit-trail rigor to small teams without enterprise operational weight. Single-binary, file-based state, SQLite, local-first, composable through open protocols.
2. **Exploration elimination.** Every question an LLM explore-agent has to spawn for is a question Clarion should have answered from cache. Batch analysis pre-computes; MCP responses stay bounded.
3. **Plugin owns ontology; core owns algorithms.** The Rust core is language-*agnostic* — no fixed enum of entity kinds, no hardcoded concept of "function" or "class." Language plugins declare the ontology they emit.
4. **Finding as fact exchange.** Findings are claims-with-evidence, not just errors: defects, structural observations, classifications, metrics, and suggestions share one record type. Findings are the suite-wide cross-tool exchange format.
5. **Observe vs. enforce is a strict boundary.** Clarion observes; Wardline enforces. Clarion's plugin detects *that* an annotation is present; Wardline determines *whether* the annotated code satisfies the semantic it declares.

### Glossary

The terms below are the ones this requirements layer uses most often:

| Term | Definition |
|---|---|
| **Briefing** | Structured summary answering a fixed set of questions about an entity. |
| **Entity** | Typed node in Clarion's property graph. |
| **Entity ID** | Stable identifier of the form `{plugin_id}:{kind}:{canonical_qualified_name}`. |
| **Edge** | Typed relationship between entities such as `contains`, `calls`, or `imports`. |
| **Finding** | Structured claim-with-evidence; may be a defect, fact, classification, metric, or suggestion. |
| **Guidance fingerprint** | Hash of the guidance sheets applied to a query; part of the summary-cache key. |
| **Plugin manifest** | YAML declaration of a plugin's kinds, edges, rules, and capabilities. |
| **Scope lens** | Query/session filter that biases neighbour lookups toward a relationship family. |
| **Tier** | Wardline trust classification preserved verbatim by Clarion. |
| **Writer-actor** | Single task that owns SQLite writes; other tasks submit mutations through it. |
| **Pre-ingest redaction** | Secret scan that runs before any file content is sent to the LLM provider. |
| **Capability probe / compat report** | Startup check of sibling-tool availability that emits one compatibility finding. |
| **`scan_run_id`** | Filigree-owned identifier for a finding-emission run. |

See [detailed-design.md](./detailed-design.md) Appendix B for the full glossary.

---

## Functional Requirements

### Catalog (`REQ-CATALOG-*`)

Producing the entity/edge/subsystem catalog from source.

#### REQ-CATALOG-01 — Entity catalog from `clarion analyze`

Clarion produces a typed entity catalog (functions, classes, modules, packages, subsystems, files, guidance sheets) from an input source tree via the `clarion analyze` command.

**Rationale**: Catalog production is the core product output — every downstream capability (briefings, MCP navigation, HTTP read API, guidance matching) reads the catalog. Without it, Clarion has nothing to serve.
**Verification**: Running `clarion analyze` against the elspeth fixture produces a populated store with ≥1 entity per known kind.
**See**: System Design §3 (Data Model), §6 (Analysis Pipeline).

#### REQ-CATALOG-02 — Plugin-declared entity kinds

Entity kinds are strings declared by a language plugin's manifest, not a fixed enum in the core. Adding a new kind (e.g., `protocol`, `dataclass`) must not require core changes.

**Rationale**: Principle 3 (plugin owns ontology). A core with a fixed entity-kind enum rots the moment a language or framework introduces a new abstraction; Clarion's structural model must survive those additions without releases.
**Verification**: Fixture test with a plugin declaring a novel kind (`test_custom_kind`) produces entities of that kind without core code changes.
**See**: System Design §2 (Core / Plugin Architecture), §3 (Data Model).

#### REQ-CATALOG-03 — Edges as typed relationships

Clarion records typed edges between entities. Core reserves `contains`, `guides`, `emits_finding`, and `in_subsystem`; all other edge kinds are plugin-declared.

**Rationale**: Edges are how navigation queries and clustering work. A language-agnostic core with a handful of reserved edges keeps the property-graph generic while giving plugins full expressive range for language-specific relationships (Python's `imports`, `calls`, `inherits_from`, `decorated_by`).
**Verification**: Plugin emits edges with plugin-defined kinds; core persists them; `neighbors(id, edge_kind=...)` returns them correctly.
**See**: System Design §3 (Data Model).

#### REQ-CATALOG-04 — First-class file entities

Files are entities in the catalog (`kind: file`) with metadata including git churn, last-modified SHA, author set, size, line count, and MIME type. File entities are parents of the code entities they contain.

**Rationale**: Files are the unit of version-control change; churn data and authorship drive staleness signals and hotspot detection. Treating files as first-class entities instead of just `source.file_path` strings makes them addressable through the same identity, finding, and briefing machinery as code.
**Verification**: Every source entity in the catalog has a `parent_id` chain resolving to a file entity; file entities have populated git metadata properties.
**See**: System Design §3 (Data Model).

#### REQ-CATALOG-05 — Subsystem entities from clustering

Clarion clusters the entity graph (imports + calls at module level) and emits one `subsystem` entity per cluster. Cluster members have `in_subsystem` edges to the subsystem.

**Rationale**: Subsystems make large codebases navigable at a higher level of abstraction than modules — an agent reasoning about "the authentication subsystem" should be able to zoom to that cluster without re-deriving its membership. Clustering is core-owned because it depends on whole-graph structure no single plugin sees.
**Verification**: `clarion analyze` on a multi-module fixture emits subsystem entities with populated members; `subsystem_members(id)` returns the clustered entity list.
**See**: System Design §3 (Data Model), §6 Phase 3 (Clustering).

#### REQ-CATALOG-06 — Symbolic entity IDs (not path-embedded)

Entity IDs follow `{plugin_id}:{kind}:{canonical_qualified_name}` for source entities; file paths are a property on the entity's `SourceRange`, not part of the ID.

**Rationale**: Cross-tool identity (Filigree issues referencing Clarion entities, Wardline findings carrying qualnames) depends on ID stability across file moves. A path-embedded ID silently detaches every reference when a file moves; canonical-name IDs survive the 80% case (file move without symbol rename).
**Verification**: Move a module on disk without renaming its symbols; run `clarion analyze`; assert that affected entity IDs are unchanged.
**See**: System Design §3 (Data Model, Identity).

#### REQ-CATALOG-07 — `first_seen_commit` and `last_seen_commit` on every entity

Each entity records the git SHA of the first run that observed it and the SHA of the most recent run that still observed it. Dirty-tree runs record the underlying commit (pre-`-dirty` suffix) so values are always real commits.

**Rationale**: Point-in-time queries ("was `TokenManager` present at SHA abc123?") without re-running analysis — enables regression-trail reconstruction and auditor-style queries.
**Verification**: Consecutive runs at different commits populate `first_seen_commit` (from the earliest run) and `last_seen_commit` (from the latest) correctly.
**See**: System Design §3 (Data Model).

---

### Analysis Pipeline (`REQ-ANALYZE-*`)

Running `clarion analyze` over a source tree.

#### REQ-ANALYZE-01 — Phased pipeline execution

The analysis pipeline executes in ordered phases (configure, structural extraction, enrichment, graph completion, clustering, leaf summarisation, module synthesis, subsystem synthesis, cross-cutting analyses, emission). Phases committing to the store must complete before dependent phases begin.

**Rationale**: Phases have strict data dependencies (clustering needs graph completion; LLM summarisation needs structural entities). Running them in the wrong order produces incoherent output; interleaving them makes resumability impossible.
**Verification**: Integration test traces phase order from `runs/<run_id>/log.jsonl` against expected sequence.
**See**: System Design §6 (Analysis Pipeline).

#### REQ-ANALYZE-02 — Parallelism within phases

Within a single phase, LLM calls execute in parallel up to a configurable cap (default 8 per model tier). Structural extraction parallelism is plugin-driven, subject to the core's writer-actor serialising commits.

**Rationale**: Serial LLM calls at elspeth scale would push wall-clock runtime above the ≤1-hour target. Parallelism-per-tier avoids overwhelming the provider's rate limits for a given model.
**Verification**: Runtime scales sub-linearly with parallelism cap on a fixture; rate-limit retries remain bounded.
**See**: System Design §6 (Analysis Pipeline, Parallelism).

#### REQ-ANALYZE-03 — Resumable after crash or interrupt

`clarion analyze --resume <run_id>` continues from the last successful phase checkpoint, skipping already-completed LLM work via content-hash caching.

**Rationale**: At elspeth scale a full run takes ~30-40 minutes and costs ~$15. A crash at 80% completion must not waste the prior 24 minutes / $12 of LLM spend. Checkpointing on phase transitions is coarse enough to avoid write amplification but fine enough to bound re-work.
**Verification**: Interrupt (SIGINT) `clarion analyze` during Phase 5; resume with `--resume`; assert phases 1-4 are skipped and Phase 5 picks up at the last completed entity.
**See**: System Design §6 (Analysis Pipeline, Resumability).

#### REQ-ANALYZE-04 — Deletion detection via entity-set diff

At Phase 7, Clarion compares the current run's entity IDs against the prior run's set and emits `CLA-FACT-ENTITY-DELETED` per missing entity, invalidates summary cache rows for deleted entities, and emits `CLA-FACT-GUIDANCE-ORPHAN` for guidance sheets pointing at deleted IDs.

**Rationale**: Without deletion detection, removed entities silently strand Filigree issues, guidance sheets, and summary-cache rows. A removed function's issues become un-actionable orphans; a removed class's guidance persists and affects briefings for entities that no longer exist. Explicit signal prevents this.
**Verification**: Run analyze, delete a file, re-run; assert a `CLA-FACT-ENTITY-DELETED` finding per previously-extracted entity in the file.
**See**: System Design §6 (Analysis Pipeline, Deletion).

#### REQ-ANALYZE-05 — Phase-7 structural findings

Clarion emits structural findings that combine signals no single sibling tool can compute alone: `CLA-FACT-TIER-SUBSYSTEM-MIXING` (cluster members in disagreeing Wardline tiers), `CLA-FACT-SUBSYSTEM-TIER-UNANIMOUS` (positive signal for tier-consistency reports), and `CLA-FACT-ENTITY-DELETED`.

**Rationale**: These rules combine Phase-3 clustering with Wardline tier declarations and prior-run state — signals Clarion uniquely holds. Emitting them from the plugin would require the plugin to know about subsystems and prior runs (Principle 3 violation); emitting from the core places them where the signals already live.
**Verification**: Fixture with mixed tiers produces `CLA-FACT-TIER-SUBSYSTEM-MIXING`; uniform-tier subsystem produces the unanimous finding.
**See**: System Design §6 (Analysis Pipeline, Phase 7).

#### REQ-ANALYZE-06 — No silent fallbacks on failure

Every recoverable failure in the pipeline emits a structured finding (`CLA-PY-PARSE-ERROR`, `CLA-INFRA-PLUGIN-CRASH`, `CLA-INFRA-LLM-ERROR`, `CLA-INFRA-BUDGET-WARNING`, etc.). No failure is silently swallowed; every finding is visible in `runs/<run_id>/stats.json`, the store, and Filigree.

**Rationale**: Silent fallbacks make debugging impossible and gradually erode trust — operators stop believing the catalog because "Clarion sometimes skips files for reasons I can't see." Explicit findings make the degradation visible and actionable.
**Verification**: Fixture with a deliberately malformed file produces `CLA-PY-PARSE-ERROR`; plugin timeout on a slow fixture produces `CLA-PY-TIMEOUT`; both reach Filigree via the normal emission path.
**See**: System Design §6 (Analysis Pipeline, Failure & Degradation).

#### REQ-ANALYZE-07 — Determinism of outputs

Back-to-back `clarion analyze` runs against identical source trees and the same recorded LLM provider produce byte-identical entity/edge/finding state (summary content may vary only if the LLM provider is non-recording).

**Rationale**: Determinism is the foundation of the diff-based developer experience — without it, every run shows spurious changes and git-committed DBs become noise. Summaries use `temperature: 0`; clustering uses a seeded RNG; phase ordering is strict.
**Verification**: Snapshot test: two sequential runs against `tests/fixtures/tiny/` produce identical `clarion db export --textual` output.
**See**: System Design §6 (Analysis Pipeline, Determinism).

---

### Entity Briefings (`REQ-BRIEFING-*`)

Structured summaries produced for each entity at policy-defined levels.

#### REQ-BRIEFING-01 — Structured, not prose

Briefings follow a fixed schema (`purpose`, `maturity`, `maturity_reasoning`, `risks`, `patterns`, `antipatterns`, `relationships`, `knowledge_basis`, `notes`). Free-form prose is not an acceptable briefing output.

**Rationale**: Principle 2 (exploration elimination) + Principle 4 (finding as fact exchange). LLM agents consume briefings as structured data to drive further navigation; prose requires re-parsing and is an order of magnitude worse for composability.
**Verification**: Every briefing in the store validates against the `EntityBriefing` schema; schema-invalid LLM responses retry once then emit `CLA-INFRA-BRIEFING-INVALID`.
**See**: System Design §3 (Data Model, Entity Briefing).

#### REQ-BRIEFING-02 — Controlled vocabulary for `patterns` / `antipatterns` / risk tags

`patterns` and `antipatterns` fields draw from a controlled vocabulary (core base set + plugin extensions). Novel tags proposed by the LLM are accepted once but logged as `CLA-FACT-VOCABULARY-CANDIDATE` for human review; they do not silently promote into future prompts.

**Rationale**: An unconstrained vocabulary is a prompt-injection vector (see `NFR-SEC-03`) and drifts across runs, undermining comparability. A controlled vocabulary gives LLMs a fixed palette while preserving extensibility for genuinely new patterns.
**Verification**: Fixture with an adversarial docstring attempting to inject a novel `antipattern` produces a `CLA-FACT-VOCABULARY-CANDIDATE`; the term is not promoted into the controlled vocabulary without human review.
**See**: System Design §3 (Data Model, Briefing), §10 (Security, Prompt-injection containment).

#### REQ-BRIEFING-03 — Summary cache keyed on content + template + tier + guidance + TTL

Generated briefings are cached by `(entity_id, content_hash, prompt_template_id, model_tier, guidance_fingerprint)`. Any key component change invalidates the cache entry; rows older than a TTL backstop (default 180 days) are invalidated unconditionally.

**Rationale**: Syntactic staleness must be impossible — guidance edits, template changes, and code content changes each correctly invalidate. TTL backstops the known semantic-staleness paths the cache key doesn't capture.
**Verification**: Edit a guidance sheet → composed fingerprint changes → dependent summary-cache rows miss on next query; content change to an entity → its cache row misses.
**See**: System Design §5 (Policy Engine, Caching).

#### REQ-BRIEFING-04 — `knowledge_basis` field per briefing

Every briefing carries `knowledge_basis: static_only | runtime_informed | human_verified`. Default is `static_only`; promotion to `human_verified` requires (a) a guidance sheet authored or reviewed against the entity in the last 90 days OR (b) a finding with `status ∈ {suppressed, acknowledged}` carrying a non-empty reason.

**Rationale**: Agents consuming briefings need to calibrate how much to trust the `risks` / `patterns` claims. A briefing derived purely from static analysis + LLM synthesis should be treated as a hypothesis; one validated by human curation (guidance or triage) earns more trust.
**Verification**: Fresh entity → briefing carries `static_only`; guidance sheet added → next briefing query carries `human_verified`; no regressed value without state change.
**See**: System Design §3 (Data Model, Briefing).

#### REQ-BRIEFING-05 — Triage-state feedback from Filigree

When rendering a briefing, Clarion queries Filigree for findings on this entity with `status ∈ {suppressed, acknowledged}` and a non-empty reason, and surfaces those as either inline notes (≤3) or a synthetic `RiskItem` with `tag: operator-acknowledged` (>3). The guidance fingerprint incorporates the set of acknowledged finding IDs.

**Rationale**: Operator triage decisions (suppressions, acknowledgements) are institutional knowledge in the same shape as guidance; they must flow back into briefings so the next LLM agent sees "this was already decided" rather than re-opening the conversation.
**Verification**: Filigree finding suppressed with a reason; next briefing for the affected entity includes the acknowledgement; cache invalidates when the triage state changes.
**See**: System Design §7 (Guidance System, Triage-state feedback).

#### REQ-BRIEFING-06 — Detail levels: short / medium / full / exhaustive

Briefings are renderable at four detail levels with token ceilings: `short` ≤100 tokens, `medium` ≤400, `full` ≤1,500, `exhaustive` ≤3,600. The ≤ figures are enforced hard limits that trigger truncation; typical briefings target roughly half these values (see System Design §3). Implementations must treat the ≤ figures as the contract.

**Rationale**: Bounded responses (Principle 2) demand fixed token budgets per detail level. LLM agents can request what they need without pulling an entire subsystem's worth of text into their context.
**Verification**: For each level, a representative fixture briefing measures under the target; exceeding triggers truncation rather than silent overflow.
**See**: System Design §3 (Data Model, Briefing), §8 (MCP, Token Budgeting).

---

### Guidance System (`REQ-GUIDANCE-*`)

Institutional knowledge attached to entities and composed into prompts.

#### REQ-GUIDANCE-01 — Guidance sheets as first-class entities

Guidance sheets are entities of `kind: guidance` with properties `{content, scope_level, scope, match_rules, expires, pinned, provenance, ...}` (per ADR-024). They participate in the same navigation, finding, and cache machinery as code entities.

**Rationale**: First-class entity status lets guidance share the identity, search, finding-emission, and MCP-tool infrastructure without parallel mechanisms. A guidance sheet can be inspected, linked to, and affected by findings in the same ways code entities are.
**Verification**: `clarion guidance create` produces an entity of `kind: guidance` with populated properties; `show_guidance(id)` returns it; findings can be emitted against it.
**See**: System Design §7 (Guidance System).

#### REQ-GUIDANCE-02 — Composition algorithm

Given `(entity_id, query_type, model_tier)`, Clarion collects candidate sheets via match-rule resolution (path, kind, tag, subsystem, wardline-group, explicit), filters by scope and expiry, sorts by `scope_rank` ASC (project → subsystem → package → module → class → function), applies a token budget (preserving `pinned: true` sheets), and returns `(segments, sheets_used, sheets_dropped, fingerprint)`.

**Rationale**: Composition order matters — inner sheets override outer with controlled precedence — and the token budget must preserve `pinned` guidance. The fingerprint determines cache invalidation and must be deterministic given the same (entity, query, tier) inputs.
**Verification**: Fixture with layered guidance (project + module + class) composes in the correct order; `pinned` sheets survive budget pressure that drops non-pinned ones; fingerprints are stable.
**See**: System Design §7 (Guidance System, Composition).

#### REQ-GUIDANCE-03 — Authoring workflows (CLI + MCP)

Operators author guidance via CLI (`clarion guidance create/edit/list/show/delete`) and via consult-mode MCP tool (`propose_guidance`). `propose_guidance` creates a Filigree *observation*, not a sheet; operator action (`clarion guidance promote <obs_id>`) is required to promote an observation into an active guidance sheet.

**Rationale**: Gating LLM-proposed guidance behind explicit promotion prevents a single adversarial prompt from poisoning every future LLM call via the summary cache (`NFR-SEC-03`). CLI authoring covers the manual human workflow; MCP proposal covers in-session agent suggestions.
**Verification**: `propose_guidance(...)` creates an observation in Filigree; active guidance sheet only exists after `guidance promote`; a compromised LLM cannot promote its own proposals.
**See**: System Design §7 (Guidance System, Authoring Workflows), §10 (Security, Guidance Promotion Gate).

#### REQ-GUIDANCE-04 — Wardline-derived guidance

On every `clarion analyze` run with `wardline.yaml` present, Clarion auto-generates `provenance: wardline_derived` guidance sheets for declared tier assignments, boundary contracts, and annotation groups in use. Auto-generated sheets carry `pinned: true`. User edits are preserved (`provenance: wardline_derived_overridden`) across regenerations.

**Rationale**: Wardline declarations (tiers, contracts) are project-wide institutional knowledge Clarion already reads at analysis time. Regenerating the corresponding guidance sheets eliminates the manual labour of keeping them in sync with `wardline.yaml`; preserving user edits respects operator curation.
**Verification**: Fixture with `wardline.yaml` produces auto-derived sheets on first run; edit a sheet; re-run; assert the edit is preserved and flagged `wardline_derived_overridden`.
**See**: System Design §7 (Guidance System, Wardline-derived).

#### REQ-GUIDANCE-05 — Staleness signals tied to code churn

For each guidance sheet, Clarion computes the aggregate `git_churn_count` delta over matched entities since the sheet's `authored_at` (or `reviewed_at`, whichever is later). Exceeding a threshold (default 50 commits; 20 for `pinned: true` sheets) emits `CLA-FACT-GUIDANCE-CHURN-STALE` with `confidence: 0.7, confidence_basis: heuristic`.

**Rationale**: Guidance is an accumulating stock with no intrinsic quality signal — especially `pinned: true` sheets, which shape LLM output most. Tying staleness to churn gives operators a review signal without auto-expiring sheets (which risks dropping still-valid guidance).
**Verification**: Fixture with high churn on matched entities produces the finding; lower threshold for pinned sheets fires earlier.
**See**: System Design §7 (Guidance System, Staleness).

#### REQ-GUIDANCE-06 — Export / import

`clarion guidance export --to <dir>` writes guidance sheets as human-readable files; `clarion guidance import <dir>` loads them back into the store. Both are deterministic (same input produces same output) and support round-tripping across Clarion installations.

**Rationale**: Guidance is committable team knowledge; export/import lets teams move guidance between machines, version it alongside code in a readable form, and review proposed guidance changes via standard diff tooling.
**Verification**: Round-trip a set of guidance sheets through export → delete from store → import → diff the restored sheets against originals; assert identical.
**See**: System Design §7 (Guidance System, Authoring Workflows).

---

### Findings (`REQ-FINDING-*`)

Emitting, classifying, and exchanging structured claims with evidence.

#### REQ-FINDING-01 — Unified Finding record

Clarion uses a single `Finding` record shape covering five kinds: `Defect`, `Fact`, `Classification`, `Metric`, `Suggestion`. Each finding carries `entity_id`, `related_entities`, `rule_id`, `message`, `evidence`, `confidence`, `confidence_basis`, and status/triage fields.

**Rationale**: Principle 4 (finding as fact exchange). A single record shape lets all tools in the Loom suite emit, consume, triage, and reason about findings uniformly — a defect, an architectural observation, and a metric all flow through the same pipe.
**Verification**: Schema test: each kind round-trips through the store and through Filigree's wire format without information loss.
**See**: System Design §3 (Data Model, Finding).

#### REQ-FINDING-02 — Namespaced rule IDs

Clarion's rule IDs follow the `CLA-*` namespace: `CLA-PY-*` for Python-plugin structural findings, `CLA-FACT-*` for factual observations, `CLA-INFRA-*` for pipeline failures, `CLA-SEC-*` for security findings. Rule IDs round-trip byte-for-byte through Filigree (`rule_id` column is free-text).

**Rationale**: Namespacing lets operators filter findings by source tool without Clarion parsing Wardline's or Semgrep's rule IDs. Free-text `rule_id` avoids enum coordination overhead while preserving social-convention namespacing.
**Verification**: Every rule Clarion emits matches its namespace prefix; a finding POSTed to Filigree returns unmodified `rule_id` on read-back.
**See**: System Design §9 (Integrations, Rule-ID round-trip).

#### REQ-FINDING-03 — Emit findings to Filigree

> **Deferred to v0.2** per the [Sprint 2 scope amendment §4](../../implementation/sprint-2/scope-amendment-2026-05.md) (WP9-B). Clarion v0.1 emits findings to its own SQLite `findings` table; cross-product emission to Filigree lands with WP9-B.

Clarion POSTs findings to Filigree via `POST /api/v1/scan-results` using Filigree's native intake schema. Clarion's richer fields (`kind`, `confidence`, `confidence_basis`, `supports`/`supported_by`, `related_entities`, internal severity, internal status) nest inside each finding's `metadata.clarion.*`.

**Rationale**: Filigree owns finding triage and lifecycle (per the Loom federation axiom — `CON-LOOM-01`). Emitting to Filigree surfaces Clarion's findings alongside Wardline's and any future scanners' in the team's unified triage view.
**Verification**: POST a Clarion finding to a mock Filigree; read back via Filigree API; assert `metadata.clarion.*` preserved verbatim; severity mapped per the spec.
**See**: System Design §9 (Integrations, Filigree).

#### REQ-FINDING-04 — General-purpose SARIF → Filigree translator

> **Deferred to v0.2** per the [Sprint 2 scope amendment §4](../../implementation/sprint-2/scope-amendment-2026-05.md) (WP10). The SARIF translator is independent of MCP surface delivery and lands with the v0.2 cross-product work.

Clarion ships `clarion sarif import <file> [--scan-source <name>]` that translates any SARIF v2.1.0 emitter (Wardline, Semgrep, CodeQL, Trivy) to Filigree's scan-results format, preserving `result.properties` into `metadata.<driver>_properties.*`.

**Rationale**: The translator is a permanent suite feature (SARIF is external, Filigree's intake is not SARIF). For v0.1 it served Wardline-to-Filigree as a side-effect; in v0.2+ Wardline ships its own native POST, but the translator stays for third-party SARIF sources.
**Verification**: SARIF corpus at `tests/fixtures/wardline-sarif/` translates to scan-results POSTs; `wardline.*` property-bag keys land in `metadata.wardline_properties.*`; severity mapped via `{error→high, warning→medium, note→info}`.
**See**: System Design §9 (Integrations, SARIF translator).

#### REQ-FINDING-05 — `scan_run_id` lifecycle

> **Deferred to v0.2** per the [Sprint 2 scope amendment §4](../../implementation/sprint-2/scope-amendment-2026-05.md) (WP9-B). Lands with REQ-FINDING-03 (findings emission to Filigree).

Clarion's `run_id` maps 1:1 onto Filigree's `scan_run_id`. Phase 0 of `clarion analyze` creates the scan run; Phase 8 closes it with `complete_scan_run=true`. Resume (`--resume`) reuses the same `run_id` and posts with `mark_unseen=false`.

**Rationale**: Run lifecycle alignment lets Filigree apply "seen-in-latest" logic correctly. Without it, resumed runs would be indistinguishable from fresh runs and prior findings would flap to `unseen_in_latest` prematurely.
**Verification**: Integration test with mock Filigree: analyze creates scan run; resumed analyze reuses the same ID; unseen transitions occur only after genuine completion.
**See**: System Design §9 (Integrations, scan_run_id).

#### REQ-FINDING-06 — Dedup policy for moved entities

> **Deferred to v0.2** per the [Sprint 2 scope amendment §4](../../implementation/sprint-2/scope-amendment-2026-05.md) (WP9-B). Concerns Filigree-side dedup; lands with REQ-FINDING-03 (findings emission).

Clarion POSTs findings with `mark_unseen=true` by default so that old-position findings for the same rule on the same file transition to `unseen_in_latest` when the entity moves within the file. `clarion analyze --prune-unseen` removes `unseen_in_latest` findings older than 30 days (configurable).

**Rationale**: Filigree's dedup key includes `line_start`; entities moving within a file produce two findings. `mark_unseen` is the v0.1 compromise — acceptable coarseness until Filigree ships server-side per-entity dedup (v0.2).
**Verification**: Fixture where a function moves from line 50 to line 80; two consecutive runs produce the expected `unseen_in_latest` state on the old finding.
**See**: System Design §9 (Integrations, Dedup).

---

### MCP Consult Surface (`REQ-MCP-*`)

The MCP tool surface exposed by `clarion serve` for consult-mode LLM agents.

#### REQ-MCP-01 — Cursor-based session model

Every MCP session has server-held state: cursor (`EntityId`), breadcrumb history, scope lens, session cost accumulator, tracked observations / proposals. Navigation tools update the cursor; inspection tools default to operating on the cursor.

**Rationale**: Cursor-based navigation eliminates the need to pass `entity_id` to every call — an agent says `goto(id)`, then `summary()`, `neighbors()`, `callers()` all operate on the current entity. Reduces parent-context token consumption (`NFR-PERF-03`) and models the "I'm looking at this entity" conversational stance naturally.
**Verification**: MCP integration test: `goto(id1)` → `summary()` → `neighbors()` all resolve against `id1` without re-passing the ID.
**See**: System Design §8 (MCP Consult Surface, Cursor model).

#### REQ-MCP-02 — Navigation and inspection tool catalogue

Clarion exposes MCP tools in the documented categories: Navigation (`goto`, `goto_path`, `back`, `zoom_out`, `zoom_in`, `breadcrumbs`); Inspection (`summary`, `source`, `metadata`, `guidance_for`, `findings_for`, `wardline_for`); Neighbours (`neighbors`, `callers`, `callees`, `children`, `imports_from`, `imported_by`, `in_subsystem`, `subsystem_members`); Search (`search_structural`, `search_semantic`, `find_by_tag`, `find_by_wardline`, `find_by_kind`); Findings & observability (`list_findings`, `emit_observation`, `promote_observation`, `cost_report`); Guidance (`show_guidance`, `list_guidance`, `propose_guidance`, `promote_guidance`); Session/scope (`set_scope_lens`, `session_info`).

**Rationale**: Principle 2 (exploration elimination) requires the tool catalogue to cover the common explore-agent questions. Missing a category forces agents to spawn sub-agents to answer what the catalogue should have answered; bloating it dilutes the surface and raises consumption.
**Verification**: For each documented tool, an MCP integration test calls it and asserts the response shape.
**See**: System Design §8 (MCP Consult Surface, Tool catalog).

#### REQ-MCP-03 — Exploration-elimination shortcuts

Clarion exposes pre-computed shortcuts operationalising common exploration queries: `find_entry_points`, `find_http_routes`, `find_cli_commands`, `find_data_models`, `find_config_loaders`, `find_tests`, `find_fixtures`, `find_deprecations`, `find_todos`, `find_dead_code`, `find_circular_imports`, `find_coupling_hotspots`, `recently_changed`, `high_churn`, `what_tests_this`. Each accepts an optional `scope` (entity ID or path glob).

**Rationale**: Each of these is an "explore-agent spawn" an LLM would otherwise perform by walking the graph — and each can be pre-computed during batch analysis and cached. Principle 2 says those spawns are a failure mode; the shortcuts are the remedy.
**Verification**: Call each shortcut on elspeth-slice fixture; assert responses populated and bounded.
**See**: System Design §8 (MCP Consult Surface, Exploration-elimination).

#### REQ-MCP-04 — Bounded response sizes

MCP tool responses respect per-tool token budgets: `summary(short) ≤100`, `summary(medium) ≤400`, `summary(full) ≤1,500`, `neighbors / callers / callees / children ≤20 results × ≤50 tokens each`, `source` paginated above 2,000 tokens, `search_*` ≤10 results. Budgets are configurable per-session via `set_budget(tool, max_tokens)`.

**Rationale**: Unbounded responses re-introduce the context-pollution problem Clarion exists to solve. Explicit budgets keep parent context growth predictable and let agents reason about how much slack they have before compaction.
**Verification**: Tool-by-tool integration test asserts responses stay within budget on representative inputs.
**See**: System Design §8 (MCP Consult Surface, Token budgeting).

#### REQ-MCP-05 — Consent gates on write-effect tools

Write-effect tools (`emit_observation`, `promote_observation`, `propose_guidance`, `promote_guidance`) return a draft for human confirmation by default. Headless agent-walk mode enables direct writes via client-declared `capabilities: { auto_emit: true }`.

**Rationale**: Consult sessions are often human-in-the-loop; surprise writes erode trust. Explicit consent via draft-then-confirm matches how human operators expect agent tools to behave; headless mode opts out for fully-automated pipelines.
**Verification**: MCP integration test: default mode returns draft; confirmation required; `auto_emit: true` skips the draft.
**See**: System Design §8 (MCP Consult Surface, Consent gates).

#### REQ-MCP-06 — Session persistence and lifecycle

Sessions are created on MCP `initialize`, idle-timeout after 1 hour (configurable), and persist to `.clarion/sessions/<id>.json` for reconnection. `clarion sessions list` and `clarion sessions close <id>` provide admin surfaces.

**Rationale**: Agents reconnecting after a transport interruption expect their cursor and breadcrumbs to survive; losing session state mid-investigation is hostile to the workflow.
**Verification**: Open a session, populate cursor, disconnect, reconnect, assert state restored.
**See**: System Design §8 (MCP Consult Surface, Session lifetime).

---

### Catalog Artefacts (`REQ-ARTEFACT-*`)

Human-readable outputs from `clarion analyze`.

#### REQ-ARTEFACT-01 — JSON catalog output

`clarion analyze` emits `.clarion/catalog.json` — a deterministic, stable-shape dump of the entity catalog, edges, subsystems, and findings at run completion.

**Rationale**: JSON is the universal interchange format; downstream consumers (dashboards, bespoke scripts, CI gates) can read the catalog without speaking SQLite. Deterministic output means git diffs reflect real changes, not run-to-run noise.
**Verification**: Two consecutive runs produce byte-identical `catalog.json`; schema conforms to a versioned JSON schema.
**See**: System Design §6 (Analysis Pipeline, Emission).

#### REQ-ARTEFACT-02 — Per-subsystem markdown + top-level index

`clarion analyze` emits `.clarion/catalog/<subsystem>.md` (one markdown file per subsystem) plus `.clarion/catalog/index.md` (top-level navigation). Markdown is generated from the store, not authored.

**Rationale**: Markdown is the human-reading surface for cases where a human (reviewer, new team member) wants to read the catalog without running `clarion serve` or speaking MCP. Subsystem granularity matches how humans think about large codebases; the index makes discovery cheap.
**Verification**: For each subsystem entity, a corresponding markdown file exists and renders cleanly; index lists all subsystems.
**See**: System Design §6 (Analysis Pipeline, Emission).

---

### Configuration / Policy Engine (`REQ-CONFIG-*`)

Reading and applying `clarion.yaml`, profiles, budgets, and caching policy.

#### REQ-CONFIG-01 — `clarion.yaml` as primary config

Clarion reads project configuration from `clarion.yaml` at the repository root, merged with user-level defaults (`~/.config/clarion/defaults.yaml`) and CLI flag overrides. The schema is versioned (`version: 1`) so breaking schema changes bump the version.

**Rationale**: Single-file project-level config matches the idioms of surrounding tooling (`pyproject.toml`, `wardline.yaml`, `.filigree/`) and keeps project policy visible in the repo. Three-tier merge order (user defaults → project → CLI) matches user expectations.
**Verification**: Fixture with clarion.yaml + user defaults + CLI override produces the expected merged config in `runs/<run_id>/stats.json`.
**See**: System Design §5 (Policy Engine, Config).

#### REQ-CONFIG-02 — Profile presets (budget / default / deep / custom)

Clarion ships three named profiles (`budget`, `default`, `deep`) and supports `custom`. Each profile specifies per-level mode / model_tier / summary_length; `clarion.yaml:llm_policy.profile` picks one; `overrides` layer on top.

**Rationale**: Named profiles make cost trade-offs legible. An operator saying "use `budget`" and getting 4× cost reduction with predictable depth loss is faster than tuning six per-level parameters; `custom` is the escape hatch for teams with specific needs.
**Verification**: Each profile produces the expected per-level configuration after merge; CLI `--profile <name>` switches profiles without editing the file.
**See**: System Design §5 (Policy Engine, Profiles).

#### REQ-CONFIG-03 — Budget enforcement with preflight

`clarion analyze` computes a cost estimate from the dry-run and confirms with the user before dispatching (default `dry_run_first: true`). During the run, budget watchers enforce `max_usd_per_run` and `max_minutes`; exceeding either with `on_exceed: stop` halts dispatch and writes a partial manifest.

**Rationale**: LLM cost surprise is a common failure mode for teams adopting LLM-assisted tooling. Preflight prevents "I just spent $400 to analyse my monorepo" incidents; in-flight enforcement bounds the worst case when estimates are wrong.
**Verification**: Estimator within ±50% on elspeth; `on_exceed: stop` with a low budget halts and writes `runs/<run_id>/partial.json`.
**See**: System Design §5 (Policy Engine, Budget).

#### REQ-CONFIG-04 — Per-level LLM policy

`clarion.yaml:llm_policy.levels.<level>` specifies `mode` (`batch | on_demand | off`), `model_tier` (`haiku | sonnet | opus`), and `summary_length`. Overrides match on `path` / `subsystem` / other criteria and layer per-level.

**Rationale**: Different entity levels (function vs subsystem) warrant different cost / depth trade-offs. Per-level policy lets operators spend Opus tokens where they matter (subsystem synthesis) and Haiku tokens where they don't (per-function leaf summaries), with path-based overrides for exceptions.
**Verification**: Config with `tests/**` → function: off filters out test-function summaries; a subsystem-specific override applies Opus to that subsystem alone.
**See**: System Design §5 (Policy Engine, Levels).

#### REQ-CONFIG-05 — Pre-ingest redaction / analysis include/exclude

Config declares `analysis.include` / `analysis.exclude` globs controlling which files analyse. Files matched by `include` are subject to pre-ingest secret scanning (`NFR-SEC-01`); excluded files never reach the LLM regardless.

**Rationale**: Include/exclude scoping is the coarse-grained boundary; secret scanning is the fine-grained one. Together they give operators confidence about what leaves the repo.
**Verification**: Files outside `include` skipped; excluded files skipped even if inside include; secret-scan flags intersect include set.
**See**: System Design §5 (Policy Engine, Config), §10 (Security).

---

### Plugin Protocol (`REQ-PLUGIN-*`)

The contract between the Rust core and language plugins.

#### REQ-PLUGIN-01 — Content-Length framed JSON-RPC 2.0

Plugins communicate with the core via Content-Length-framed JSON-RPC 2.0 over stdio, matching LSP's framing convention. Each message is prefixed with `Content-Length: <n>\r\n\r\n`.

**Rationale**: Content-Length framing is binary-safe (no newline ambiguity in JSON content) and resumable after partial reads — essential for crash-recovery semantics. Newline-delimited JSON is tempting but breaks on embedded newlines in content fields.
**Verification**: Protocol compliance test: malformed frames (missing length, wrong length) fail cleanly; partial reads reassemble correctly.
**See**: System Design §2 (Core / Plugin Architecture, Protocol).

#### REQ-PLUGIN-02 — Plugin manifest declares ontology

Each plugin ships a manifest declaring its entity kinds, edge kinds, tags, capabilities (e.g., `calls`, `imports`, `inherits_from`), supported rule IDs, and prompt templates. The core refuses to run with a manifest missing required fields.

**Rationale**: Principle 3 — ontology lives in the plugin. The manifest is the contract: the core knows what to expect from the plugin's emissions without hardcoding per-language knowledge.
**Verification**: Fixture plugin with invalid manifest rejected at startup; valid manifest's declared kinds appear in emitted entities.
**See**: System Design §2 (Core / Plugin Architecture, Manifest).

#### REQ-PLUGIN-03 — Lifecycle methods (analyze, build_prompt)

Plugins implement two phases of lifecycle calls: batch (`initialize`, `file_list`, `analyze_file(path) → stream of entities + edges + findings`) and consult (`build_prompt(entity_id, query_type, context)`). Calls are JSON-RPC methods; streams use a separate `file_analyzed` notification channel.

**Rationale**: Splitting lifecycle into batch and consult lets plugins optimise each independently — batch is throughput-oriented; consult is latency-oriented. Streaming from `analyze_file` lets the core commit entities incrementally rather than buffering a whole file's worth.
**Verification**: Fixture plugin responds to each method; streaming behaviour commits entities as they arrive.
**See**: System Design §2 (Core / Plugin Architecture, Lifecycle).

#### REQ-PLUGIN-04 — Python plugin (v0.1)

Clarion ships a Python plugin supporting Python ≥3.11 that extracts functions, classes, protocols, globals, modules, packages, and their edges (`imports`, `calls`, `inherits_from`, `decorated_by`, `uses_type`, `alias_of`). Installation via `pipx install clarion-plugin-python` for isolation.

**Rationale**: Python is the validating first-customer language (elspeth is ~425k LOC Python). Shipping the plugin alongside the core for v0.1 establishes the plugin-authoring contract and validates the plugin protocol against a real workload; `pipx` isolation prevents venv conflicts with the analysed project.
**Verification**: `tests/fixtures/elspeth-slice/` runs through the Python plugin and produces expected entity/edge counts; installation via pipx succeeds.
**See**: System Design §2 (Python plugin specifics).

#### REQ-PLUGIN-05 — Python import resolution policy

The Python plugin resolves imports per a declared policy: `sys.path` discovered via virtualenv introspection or user-supplied `python_executable`; `src.` prefix stripped by default; `__init__.py` re-exports become `alias_of` edges (definition site wins); unresolvable imports produce `python:unresolved:{module.path}` placeholder entities; `TYPE_CHECKING` blocks excluded from runtime-import edges.

**Rationale**: Python's import model is the single hardest static-analysis problem at elspeth scale; leaving it undefined produces an entity graph that is subtly wrong in different ways on different installations. An explicit policy makes the behaviour testable and predictable.
**Verification**: Fixture with each import shape produces the documented resolution; `TYPE_CHECKING` imports don't generate spurious circular-import findings.
**See**: System Design §2 (Python plugin specifics, Import resolution).

#### REQ-PLUGIN-06 — Decorator detection policy

The Python plugin detects decorators including factory invocations (`@app.route("/health")`), stacked decorators (preserving order — matters for Wardline semantics), class decorators, and aliases (`validates = validates_shape`). Each decoration produces a `decorated_by` edge with optional `properties` capturing decorator arguments.

**Rationale**: Decorator-as-DSL is widespread in Python (FastAPI, Pydantic, Wardline itself). Naive direct-name matching misses most decorator usage; explicit handling makes the entity metadata faithful to what the code actually declares.
**Verification**: Fixture with each decorator shape produces the expected `decorated_by` edges with preserved argument metadata.
**See**: System Design §2 (Python plugin specifics, Decorator detection).

---

### HTTP Read API (`REQ-HTTP-*`)

The read-only HTTP surface exposed by `clarion serve`.

#### REQ-HTTP-01 — Read endpoints for entities, findings, wardline, state

Clarion exposes read-only HTTP endpoints: `GET /api/v1/entities`, `GET /api/v1/entities/{id}`, `GET /api/v1/entities/{id}/neighbors`, `GET /api/v1/entities/{id}/summary`, `GET /api/v1/entities/{id}/guidance`, `GET /api/v1/entities/{id}/findings`, `GET /api/v1/findings`, `GET /api/v1/wardline/declared`, `GET /api/v1/state`, `GET /api/v1/health`, `GET /api/v1/metrics` (Prometheus-compatible).

**Rationale**: Sibling tools (Wardline in v0.2+, future dashboards, CI gates) consume Clarion's catalog via HTTP; MCP is not appropriate for cross-process state pulls. Read-only in v0.1 keeps the surface small.
**Verification**: Contract test per endpoint against a fixture catalog; `/metrics` scrapes cleanly; `/health` returns the expected envelope.
**See**: System Design §9 (Integrations, HTTP Read API).

#### REQ-HTTP-02 — Entity resolution oracle

`GET /api/v1/entities/resolve?scheme=<scheme>&value=<value>` translates from sibling-tool identity schemes (`wardline_qualname`, `wardline_exception_location`, `file_path`, `sarif_logical_location`) to Clarion entity IDs. Returns `{entity_id, kind, resolution_confidence: exact|heuristic|none, alternatives}`.

**Rationale**: Enrichment-not-load-bearing (`CON-LOOM-01`): sibling tools consuming Clarion should ask in *their* native identity scheme, not embed Clarion's ID format. `resolve` exposes Clarion's internal translation layer as a public API so every sibling doesn't re-implement it.
**Verification**: Contract test for each scheme; `resolution_confidence: none` returned as 200 with empty `entity_id` (not 404) so callers can distinguish absent-from-catalog vs. server-down.
**See**: System Design §9 (Integrations, Entity resolve).

#### REQ-HTTP-03 — Registry-backend HTTP trust model

For Filigree `registry_backend: clarion`, Clarion's HTTP read API is
loopback-only by default. Non-loopback binds require **both**
`serve.http.allow_non_loopback: true` **and** a resolved authentication secret
— either HMAC identity via `serve.http.identity_token_env` (preferred per
ADR-034) or a legacy bearer token via `serve.http.token_env`. A non-loopback
bind with the opt-in but no resolved secret is refused at startup with
`CLA-CONFIG-HTTP-NO-AUTH`. The loopback-without-token mode remains
unauthenticated and emits a startup warning that any local process can read the
catalogue; non-loopback no longer has an unauthenticated mode.

**Rationale**: the ADR-014 read surface is a bounded local federation contract,
not the earlier broad HTTP API. ADR-034 closes the original gap that permitted
unauthenticated non-loopback binds behind the `allow_non_loopback` opt-in
alone. The implementation prevents accidental network exposure mechanically and
makes intentional non-loopback exposure fail closed without an authentication
secret rather than silently warn-and-bind.
**Verification**: `crates/clarion-cli/tests/serve.rs` covers the loopback
default (line 1457), the loopback `identity_token_env`-resolution-failure
refusal (line 1495), the non-loopback-without-auth startup refusal (line 1547),
the non-loopback HMAC-required path (line 1579), and the non-loopback
legacy-bearer path (line 1614). The loopback startup-warning surface is covered
by config-layer tests.
**See**: System Design §9 (Integrations, HTTP Read API), ADR-014, ADR-034.

#### REQ-HTTP-04 — ETag-style response caching

Every HTTP response carries `X-Clarion-State: <hash>` for client-side caching. Clients can supply `If-None-Match` with the previously-received state hash; unchanged → 304.

**Rationale**: Wardline-style consumers poll at commit cadence; cheap cache revalidation reduces load on Clarion and network cost for the consumer. `X-Clarion-State` is a run-level hash (not per-entity) for simplicity; finer-grained caching is v0.2+.
**Verification**: Contract test: second request with matching state hash returns 304 with no body.
**See**: System Design §9 (Integrations, HTTP Read API).

---

### Filigree Integration (`REQ-INTEG-FILIGREE-*`)

Clarion's side of Filigree integration.

#### REQ-INTEG-FILIGREE-01 — Findings via scan-results intake

> **Deferred to v0.2** per the [Sprint 2 scope amendment §4](../../implementation/sprint-2/scope-amendment-2026-05.md) (WP9-B). Lands with REQ-FINDING-03.

Clarion POSTs findings to Filigree's `POST /api/v1/scan-results` using the native schema (see `REQ-FINDING-03`) with `scan_source: "clarion"`. Clarion inspects `response.warnings[]` on every POST for silent coercion / unknown-key drops.

**Rationale**: Filigree's scan-results intake is the battle-tested cross-tool finding-exchange path; the warnings array is how Filigree signals schema drift. Ignoring warnings means silently shipping malformed findings.
**Verification**: Mock Filigree returns synthetic warnings; Clarion logs them at WARN and emits `CLA-INFRA-FILIGREE-WARNINGS`.
**See**: System Design §9 (Integrations, Filigree — Finding exchange).

#### REQ-INTEG-FILIGREE-02 — Observation emission (HTTP preferred, MCP fallback)

> **Deferred to v0.2** per the [Sprint 2 scope amendment §4](../../implementation/sprint-2/scope-amendment-2026-05.md) (WP9-B). Clarion v0.1 surfaces structural and security findings via its own MCP `issues_for` tool (WP9-A); cross-product observation emission to Filigree lands with WP9-B.

Clarion emits observations to Filigree via `POST /api/v1/observations` when available; falls back to MCP-client transport (spawning `filigree mcp` as subprocess) when the HTTP endpoint is absent. The fallback is signalled in the capability compat report.

**Rationale**: Observations are fire-and-forget notes; Clarion generates them during analyse and consult. HTTP is the natural transport for a Rust binary emitting to a local Filigree; MCP is the v0.1 workaround while Filigree adds the HTTP endpoint.
**Verification**: HTTP path used by default; `--no-filigree-http` flag forces MCP fallback; both paths produce observations visible in Filigree.
**See**: System Design §9 (Integrations, Filigree — Observations), §11 (Suite Bootstrap).

#### REQ-INTEG-FILIGREE-03 — Registry-backend consumption

> **Deferred to v0.2** per the [Sprint 2 scope amendment §4](../../implementation/sprint-2/scope-amendment-2026-05.md) (WP9-B/WP10) and reflected in [CON-FILIGREE-02](#con-filigree-02--file-registry-displacement-is-deferred-to-v02). Filigree's `registry_backend` flag shipped (ADR-014); Clarion's read-side `RegistryProtocol` implementation is the v0.2 work. Clarion v0.1 ships shadow-registry only.

When Filigree's `registry_backend` flag is set to `clarion`, Clarion serves as Filigree's file registry: Filigree consults Clarion's HTTP read API for file ID resolution; auto-create paths route through `RegistryProtocol` to Clarion. Absent the flag, Clarion operates in shadow-registry mode (findings POSTed normally; Filigree auto-creates `file_records` under its native rules).

**Rationale**: File-registry displacement is the cleanest expression of "Clarion owns structural truth" (per Loom federation). The shadow-registry fallback preserves v0.1 shipability when Filigree hasn't yet landed `registry_backend`.
**Verification**: Mock Filigree with flag set → Clarion serves `GET /api/v1/entities/resolve?scheme=file_path` in response to Filigree resolution calls; flag absent → shadow mode, compat report flags degradation.
**See**: System Design §9 (Integrations, Filigree — Registry), §11 (Suite Bootstrap, Prerequisites named here).

#### REQ-INTEG-FILIGREE-04 — `scan_source` namespace + schema pin test

> **Deferred to v0.2** per the [Sprint 2 scope amendment §4](../../implementation/sprint-2/scope-amendment-2026-05.md) (WP9-B). Lands with REQ-FINDING-03 (findings emission to Filigree); schema-pin test is moot until Clarion is actually POSTing.

Clarion uses `scan_source: "clarion"` for emissions; CI runs a schema-compatibility test against a Filigree release's `GET /api/files/_schema` output to detect drift in `valid_severities`, `valid_finding_statuses`, `valid_association_types`.

**Rationale**: Filigree's CHANGELOG flags breaking API changes but relies on social discipline; a pinned schema-compat test gives Clarion CI-level protection against silent schema shifts.
**Verification**: CI job against a tagged Filigree release passes; modifying the fixture schema fails the test.
**See**: System Design §9 (Integrations, Filigree — Schema contract).

#### REQ-INTEG-FILIGREE-05 — Capability-negotiation probe

> **Deferred to v0.2** per the [Sprint 2 scope amendment §4](../../implementation/sprint-2/scope-amendment-2026-05.md) (WP9-B). The capability probe is meaningful only once Clarion is talking to Filigree via the v0.2 cross-product surfaces (REQ-FINDING-03, REQ-INTEG-FILIGREE-02/03).

At `clarion analyze` startup, Clarion probes Filigree's presence, version, `registry_backend` setting, and `/api/v1/observations` availability via `GET /api/files/_schema` + `HEAD` checks. Results emit in a single `CLA-INFRA-SUITE-COMPAT-REPORT` finding.

**Rationale**: Partial Filigree versions are common (deployment skew). A single compat report collapses scattered runtime surprises into one auditable signal operators can read at the start of each run.
**Verification**: Mock Filigree with varied capability responses; compat report correctly reflects each.
**See**: System Design §11 (Suite Bootstrap, Capability probe).

---

### Wardline Integration (`REQ-INTEG-WARDLINE-*`)

Clarion's side of Wardline integration (v0.1 is read-only ingest).

#### REQ-INTEG-WARDLINE-01 — Direct REGISTRY import with version pin

Clarion's Python plugin imports `wardline.core.registry.REGISTRY` at startup and pins against an expected `REGISTRY_VERSION`. Additive-newer versions proceed with a warning; major-bump or older falls back to a hardcoded registry mirror (`wardline_registry_v<pin>.py`) with `CLA-INFRA-WARDLINE-REGISTRY-MIRRORED`.

**Rationale**: Direct import is cheaper than file-descriptor reading and avoids a vocabulary-drift window where two tools have different understandings of decorator semantics. Version pinning plus mirror fallback preserves operation when skew occurs.
**Verification**: Matching version → normal operation; additive skew → warning; major skew → mirror mode.
**See**: System Design §2 (Core / Plugin Architecture, Direct REGISTRY import), §11 (Suite Bootstrap, Prerequisites named here).

#### REQ-INTEG-WARDLINE-02 — Manifest + overlay ingest

Clarion reads `wardline.yaml` and overlay files matching `src/**/wardline.overlay.yaml` at analyse time; declared tiers, groups, and boundary contracts become `WardlineMeta` properties on affected entities.

**Rationale**: The manifest is Wardline's declarative source of truth; ingesting it makes tier/group/contract declarations available as entity metadata without re-implementing Wardline's parsing. Overlays let per-subsystem declarations compose cleanly.
**Verification**: Fixture with manifest + overlays produces entities with correct `declared_tier`, `declared_groups`, `declared_boundary_contracts`.
**See**: System Design §9 (Integrations, Wardline).

#### REQ-INTEG-WARDLINE-03 — Fingerprint ingest

Clarion reads `wardline.fingerprint.json` at analyse time; each per-function `FingerprintEntry` becomes `WardlineMeta.annotation_hash` + `wardline_qualname` on the resolved entity. Unresolved fingerprint entries emit `CLA-INFRA-WARDLINE-FINGERPRINT-UNRESOLVED`.

**Rationale**: Fingerprint provides the authoritative per-function annotation hash — without it, Clarion can't track drift between what Wardline enforces and what the code declares. Resolution failures must be visible (not silent) so operators can fix the mapping.
**Verification**: Fingerprint entries for known entities populate `annotation_hash`; deliberate mis-resolution emits the finding.
**See**: System Design §9 (Integrations, Wardline — Fingerprint).

#### REQ-INTEG-WARDLINE-04 — Exceptions ingest

Clarion reads `wardline.exceptions.json` at analyse time; entities referenced by active exceptions are tagged `wardline.excepted`. Unresolvable exception `location` strings emit `CLA-INFRA-WARDLINE-EXCEPTION-UNRESOLVED` and persist as dangling records with `entity_id: null`.

**Rationale**: Exceptions are operator-curated decisions ("this finding is accepted"). Agents reading briefings for excepted entities should see "this has an active exception" as part of the picture; unresolvable exceptions are operator bugs that need visibility.
**Verification**: Exception entries tag resolved entities with `wardline.excepted`; unresolvable entries produce the finding.
**See**: System Design §9 (Integrations, Wardline — Exceptions).

#### REQ-INTEG-WARDLINE-05 — SARIF baseline ingest for translator

Clarion reads `wardline.sarif.baseline.json` (read-only) for the `clarion sarif import` translator path — the 663-result baseline is the source for Wardline-to-Filigree finding flow in v0.1.

**Rationale**: Translator ownership lives Clarion-side in v0.1 (ADR-015); reading the baseline from disk keeps Wardline's dependency graph unchanged until it ships a native Filigree emitter in v0.2+.
**Verification**: `clarion sarif import wardline.sarif.baseline.json --scan-source wardline` produces expected Filigree POST payload.
**See**: System Design §9 (Integrations, SARIF translator).

#### REQ-INTEG-WARDLINE-06 — Identity reconciliation across three schemes

Clarion maintains translation between three identity schemes: Clarion `EntityId`, Wardline `qualname`, Wardline exception-register `location` string. Reconciliation uses Wardline's `module_file_map` (from `ScanContext`) plus parsed location strings.

**Rationale**: The three schemes arose independently and are not byte-equal for the same symbol. Clarion is the translator (Principle 3 — Wardline keeps its scheme; Clarion produces the join); exposing the translation via `GET /api/v1/entities/resolve` (`REQ-HTTP-02`) lets sibling tools consume it without embedding Clarion's ID format.
**Verification**: Given a Wardline qualname + file, `resolve` returns the correct Clarion entity ID; given an exception location string, same.
**See**: System Design §3 (Data Model, Identity reconciliation).

---

## Non-Functional Requirements

### Performance (`NFR-PERF-*`)

#### NFR-PERF-01 — Elspeth-scale wall-clock budget

`clarion analyze /home/john/elspeth` completes in ≤60 minutes (target ~38 minutes) on a representative developer machine (8+ cores, SSD, ≥16GB RAM).

**Rationale**: 60 minutes is the psychological ceiling for a "run overnight or during lunch" batch tool; significantly above it pushes teams to run less often and lose feedback. The ~38-minute target is derived from the detailed-design's example run; the ceiling bounds estimation error.
**Verification**: Full elspeth run measured end-to-end; time logged in `runs/<run_id>/stats.json`.
**See**: System Design §6 (Analysis Pipeline, Parallelism).

#### NFR-PERF-02 — MCP response latency

`clarion serve`'s MCP initialize completes in ≤100ms. Cached MCP tool calls (hot entities) return in ≤50ms p95. Cache-miss `summary()` calls bounded by LLM latency but must not block other concurrent MCP tool calls.

**Rationale**: Interactive consult-mode latency shapes agent-workflow UX; 100ms initialise keeps session spin-up invisible. Cache-hit latency determines whether Clarion feels like "instant lookup" or "async service"; agents giving up on slow tools is a failure mode.
**Verification**: Microbenchmark harness against `tests/fixtures/moderate/`; p95 latencies logged.
**See**: System Design §8 (MCP Consult Surface, Token budgeting — implementation).

#### NFR-PERF-03 — Parent context growth per MCP call

A Claude Code consult session navigating 20+ turns grows parent context by ≤500 tokens per Clarion tool call on average (summary short + neighbours short composition).

**Rationale**: Principle 2 (exploration elimination) fails if Clarion's responses pollute the parent as much as explore-subagents do. Bounded growth per call keeps long consult sessions viable within a single conversation window.
**Verification**: Integration test with a recorded 20-turn consult session; average growth per call measured.
**See**: System Design §8 (MCP Consult Surface).

---

### Scale (`NFR-SCALE-*`)

#### NFR-SCALE-01 — Elspeth validation (~425k LOC Python)

Clarion v0.1 is validated against `elspeth` (~425k LOC Python, ~1,100 files). The system handles that scale with the entity count (~100-200k entities, ~500k-1M edges) expected from that input.

**Rationale**: Elspeth is the validating first customer — the scale wall where the Claude Code archaeologist skill broke. Clarion's purpose is crossing that wall; meeting elspeth is the minimum viable product proof.
**Verification**: Full elspeth run produces expected entity / edge counts within ±20%; no OOM on a 16GB machine.
**See**: System Design §4 (Storage, Scale estimate).

#### NFR-SCALE-02 — DB size bound

The `.clarion/clarion.db` store for an elspeth-scale project fits within 2GB. Larger projects degrade gracefully — no hard cap, but cost of commit-the-DB grows.

**Rationale**: Committed DBs live in git; a 2GB DB is uncomfortable but workable, 10GB is pathological. Matching elspeth to 500MB-2GB keeps the commit-DB story honest.
**Verification**: Elspeth run produces a DB within the bound; DB growth linear with entity count.
**See**: System Design §4 (Storage, Scale estimate).

#### NFR-SCALE-03 — Read-connection pool saturation

16 concurrent read connections (`deadpool-sqlite` default) handle the MCP + HTTP load produced by one consult-mode agent + one Wardline-equivalent state puller without saturation.

**Rationale**: A single-machine deployment should not need tuning to handle a realistic consumer load. If 16 connections isn't enough for the default scenario, the default is wrong.
**Verification**: Load test with concurrent MCP + HTTP traffic; measure connection-pool exhaustion events (expected: zero).
**See**: System Design §4 (Storage, Concurrency).

---

### Security (`NFR-SEC-*`)

#### NFR-SEC-01 — Pre-ingest secret scanning

Before any file content reaches the LLM provider, Clarion runs a pre-ingest secret scanner (bundled `detect-secrets` or equivalent) on the file buffer. Unredacted secrets emit `CLA-SEC-SECRET-DETECTED` and **block LLM dispatch for that file**. False-positive whitelist at `.clarion/secrets-baseline.yaml`.

**Rationale**: The first real user running `clarion analyze` on a repo with a committed `.env` would otherwise silently leak to Anthropic. Pre-ingest redaction is a hard dependency for v0.1; retrofitting it after a leak is too late.
**Verification**: Fixture with a deliberately-committed test secret blocks LLM dispatch; baseline whitelist suppresses the block for approved false positives.
**See**: System Design §10 (Security, Pre-ingest redaction).

#### NFR-SEC-02 — Prompt-injection containment

Clarion structures prompts with explicit `<file_content trusted="false">...</file_content>` delimiters; briefing outputs are validated against the `EntityBriefing` JSON schema; `patterns` / `antipatterns` are controlled vocabulary; `propose_guidance` creates observations (not sheets) requiring manual promotion; `knowledge_basis: static_only` flags briefings derived solely from LLM output.

**Rationale**: Adversarial docstrings / comments / string literals can attempt to inject instructions into the summarisation prompt or propose attacker guidance that lands in every future prompt (cache poisoning). Layered defence is required because no single mechanism is sufficient.
**Verification**: Fixture with adversarial docstring; briefing schema-validates; `propose_guidance` call produces an observation, not a sheet; novel vocabulary surfaces as `CLA-FACT-VOCABULARY-CANDIDATE`.
**See**: System Design §10 (Security, Prompt-injection).

#### NFR-SEC-03 — Non-loopback HTTP read API guard

Clarion refuses to start the HTTP read API on a non-loopback bind unless **both**
`serve.http.allow_non_loopback: true` is set **and** an authentication secret is
resolved at startup — either an HMAC identity secret via
`serve.http.identity_token_env` (preferred, per ADR-034) or a legacy bearer
token via `serve.http.token_env`. A non-loopback bind with neither secret
resolved fails closed at startup with `CLA-CONFIG-HTTP-NO-AUTH`. The
startup-warning surface (endpoint unauthenticated; protect outside Clarion)
applies only to the loopback-without-token mode, which remains the ADR-014
default for the local sidecar case.

**Rationale**: loopback is not a complete security boundary, but accidental
network *exposure* without authentication is the highest-risk failure mode for
the registry-backend surface. ADR-034 closes the original ADR-014 gap that
permitted unauthenticated non-loopback binds behind the `allow_non_loopback`
opt-in alone; the opt-in remains the gate that admits non-loopback binds at all
but no longer admits them unauthenticated.
**Verification**: `crates/clarion-cli/tests/serve.rs` covers the loopback
default (line 1457), the loopback `identity_token_env`-set-but-env-missing
refusal (line 1495), the non-loopback-without-auth refusal (line 1547), the
non-loopback HMAC-required path (line 1579), and the non-loopback legacy-bearer
path (line 1614). Config-layer tests cover the loopback startup-warning
surface.
**See**: System Design §9 (HTTP Read API), §10 (Security), ADR-014, ADR-034.

#### NFR-SEC-04 — Audit surface — security events as findings

Every security-relevant event (`CLA-SEC-SECRET-DETECTED`, `CLA-SEC-UNREDACTED-SECRETS-ALLOWED`, `CLA-INFRA-TOKEN-STORAGE-DEGRADED`, `CLA-INFRA-BRIEFING-INVALID`, `CLA-SEC-VOCABULARY-CANDIDATE-NOVEL`) emits a finding that reaches Filigree via the normal exchange.

**Rationale**: Security observability is part of the normal finding flow — no separate audit subsystem. Operators running `filigree list --label=security --since 7d` across the suite see Clarion's events alongside Wardline's and any future scanners'.
**Verification**: Each event type produces a finding in Filigree when triggered.
**See**: System Design §10 (Security, Audit Surface).

#### NFR-SEC-05 — Run log exclusion from git by default

`runs/<run_id>/log.jsonl` (raw LLM request/response bodies) is git-excluded by default via `.clarion/.gitignore` (`runs/*/log.jsonl`). Operators opt-in to committing explicitly.

**Rationale**: Run logs may contain source excerpts appropriate to ship to Anthropic but not appropriate to commit to a public repo. Default-exclude prevents accidental exposure; explicit opt-in forces the operator to own the choice.
**Verification**: Fresh install produces `.clarion/.gitignore` with the rule; log files not tracked in the next `git status`.
**See**: System Design §4 (Storage, Commit posture), §10 (Security, Operator guidance).

---

### Operational Posture (`NFR-OPS-*`)

#### NFR-OPS-01 — Single-binary distribution

Clarion core distributes as a single native binary per target (Linux x86_64, Linux ARM64, macOS x86_64, macOS ARM64, Windows x86_64). No dynamic linking beyond libc; no required runtime dependencies.

**Rationale**: Principle 1 (enterprise at lack of scale). Small teams don't have platform engineers; "download and run" is the deployment target. Dynamic dependencies re-introduce the platform-team problem.
**Verification**: Binary on each target runs on a fresh install without installing additional dependencies; startup succeeds.
**See**: [detailed-design.md](./detailed-design.md) Appendix C (Rust stack), System Design §12 (Architecture Decisions — ADR-001).

#### NFR-OPS-02 — Local-first; no cloud dependency

Clarion runs entirely locally. The only required network egress is the LLM provider API during `clarion analyze` summarisation phases. No telemetry, no crash-reporting phone-home, no license-server callback.

**Rationale**: Enterprise-at-lack-of-scale means not forcing a hosted service on users. Adopting Clarion must not require signing a cloud agreement.
**Verification**: Network egress audit during `clarion analyze`: only Anthropic endpoints in the packet capture.
**See**: System Design §1 (Context & Boundaries).

#### NFR-OPS-03 — `.clarion/` git-committable

The `.clarion/` directory (including `clarion.db` by default) is safe to commit to git. Textual DB export (`clarion db export --textual`) and a merge helper (`clarion db merge-helper`) handle multi-developer conflicts.

**Rationale**: Shared analysis state benefits small teams (one developer pays the LLM cost; the team sees the briefings). Commit-by-default matches Filigree's and Wardline's storage patterns. Textual export makes git diffs meaningful.
**Verification**: `git add .clarion && git commit` succeeds on a populated store; two developers' simultaneous runs produce a DB that the merge helper resolves deterministically.
**See**: System Design §4 (Storage, File layout).

#### NFR-OPS-04 — Python plugin install via pipx

The Python plugin installs via `pipx install clarion-plugin-python` into its own venv. Clarion's `plugins.toml` records the plugin's executable path and Python version.

**Rationale**: Installing the plugin into the analysed project's venv causes dependency conflicts (Clarion's plugin dependencies can collide with the project's). pipx isolation sidesteps this at the cost of an extra install step — acceptable tradeoff.
**Verification**: `pipx install` succeeds against a fresh Python 3.11 environment; plugin loads and emits entities for the tiny fixture.
**See**: System Design §2 (Python plugin specifics, Packaging).

---

### Observability (`NFR-OBSERV-*`)

#### NFR-OBSERV-01 — Structured JSON logs

Clarion emits structured JSON-line logs via the `tracing` crate. Logs rotate at 100MB with 5 files kept. Per-run log at `.clarion/runs/<run_id>/log.jsonl`; per-process log at `.clarion/clarion.log`.

**Rationale**: Structured logs are machine-parseable; text logs aren't. Downstream log aggregation (if operators route Clarion's output into Vector / Loki / Splunk) works by default.
**Verification**: Log entries parse as JSON; rotation verified; log levels respected.
**See**: System Design §5 (Policy Engine, Observability).

#### NFR-OBSERV-02 — Per-run `stats.json`

Each `clarion analyze` run writes `runs/<run_id>/stats.json` with total cost, per-level LLM cost breakdown, per-model breakdown, cache hit rate, phase durations, finding counts, failure counts, and compat-probe result.

**Rationale**: Post-run introspection without grepping logs. Cost, cache hit rate, and failure counts are the first three things an operator asks about after a run.
**Verification**: `stats.json` schema validates; values match what logs record.
**See**: System Design §5 (Policy Engine, Observability).

#### NFR-OBSERV-03 — Prometheus-compatible `/api/v1/metrics`

`clarion serve` exposes `/api/v1/metrics` in Prometheus text format, covering MCP request counts, HTTP request counts by endpoint and status, cache hit/miss counts, session counts, active LLM call count.

**Rationale**: Operators running Clarion as a long-lived service need metrics; Prometheus is the ubiquitous standard. Exposing without a separate exporter means small teams can point Prometheus at Clarion directly.
**Verification**: `curl /api/v1/metrics` returns valid Prometheus text; key metrics present after a load-test.
**See**: System Design §9 (HTTP Read API).

#### NFR-OBSERV-04 — `CLA-INFRA-SUITE-COMPAT-REPORT` at every analyse

Every `clarion analyze` emits exactly one `CLA-INFRA-SUITE-COMPAT-REPORT` finding summarising the capability-probe results (Filigree presence, version, flags; Wardline REGISTRY version; SARIF schema version; degraded paths active).

**Rationale**: One finding collapses scattered runtime signals. Operators asking "why did this run behave differently from last week?" check the compat report first.
**Verification**: Every run produces exactly one such finding in the run's finding set.
**See**: System Design §11 (Suite Bootstrap, Capability probe).

---

### Cost (`NFR-COST-*`)

#### NFR-COST-01 — Elspeth run budget target

`clarion analyze /home/john/elspeth` costs $15 ± 50% in LLM spend (range: $7.50 - $22.50) at default profile with current Anthropic pricing.

**Rationale**: Matches the detailed-design's example run. The ±50% band reflects estimator uncertainty (subsystem synthesis cost varies with clustering) plus pricing volatility; wider bands undermine operator trust.
**Verification**: Full elspeth run measures total cost; repeat runs within the band.
**See**: System Design §5 (Policy Engine, Budget).

#### NFR-COST-02 — Cache hit rate after stabilisation

After three consecutive runs without source or guidance changes, summary cache hit rate is ≥95% for subsequent runs.

**Rationale**: High cache hit rate is the primary cost-control mechanism. If the cache key design is wrong (over-invalidating), re-runs cost nearly as much as first runs and the tool becomes prohibitive.
**Verification**: Run elspeth three times unchanged; measure hit rate on run 4.
**See**: System Design §5 (Policy Engine, Caching).

#### NFR-COST-03 — Preflight cost estimate accuracy ±50%

The dry-run cost estimate is within ±50% of actual spend on representative projects. Systematic under-estimation is worse than over-estimation (preflight should not encourage false confidence).

**Rationale**: An estimate outside this range fails its purpose (preventing cost surprise). ±50% is tight enough to be useful, loose enough to be achievable with per-entity-level heuristics rather than full Opus pricing simulations.
**Verification**: Elspeth dry-run estimate vs. actual; repeat across varied fixture projects.
**See**: System Design §5 (Policy Engine, Budget — Preflight).

---

### Reliability (`NFR-RELIABILITY-*`)

#### NFR-RELIABILITY-01 — Crash-surviving store

`.clarion/clarion.db` survives unclean shutdown (SIGKILL during analyze) without corruption. Subsequent `clarion analyze --resume <run_id>` continues from the last checkpoint.

**Rationale**: SQLite WAL + writer-actor per-N-files transactions + checkpoint discipline produce crash-safe semantics when configured correctly. Getting this right in v0.1 is non-negotiable — a corrupt store costs the user everything.
**Verification**: Test harness: `clarion analyze` → `kill -9` mid-run → next invocation loads the DB cleanly → `--resume` continues.
**See**: System Design §4 (Storage, Concurrency).

#### NFR-RELIABILITY-02 — Degraded modes for missing siblings

`clarion analyze --no-filigree` (Filigree unreachable) writes findings to `runs/<run_id>/findings.jsonl` locally and continues. `clarion analyze --no-wardline` skips Wardline state ingest and continues. Missing `clarion sarif import` doesn't block `clarion analyze`.

**Rationale**: Clarion ships on its own timeline, not the slowest of three (`CON-LOOM-01` — enrichment-not-load-bearing). Explicit flags document the degradation; they're not silent fallbacks.
**Verification**: Each flag produces a successful run with the corresponding feature disabled; per-flag compat-report entry.
**See**: System Design §11 (Suite Bootstrap, Per-component fallbacks).

#### NFR-RELIABILITY-03 — No silent failure

Every failure category (plugin parse error, plugin timeout, plugin crash, LLM rate limit, LLM non-transient error, budget exceeded, schema-invalid LLM response, plugin crash-loop) emits a structured finding. No failure is silently swallowed.

**Rationale**: Silent failures erode trust faster than explicit ones. An operator hitting a finding knows there's a problem; one hitting silently-missing briefings doesn't know to look.
**Verification**: Fixture-driven failure-injection test per category; each produces the expected finding.
**See**: System Design §6 (Analysis Pipeline, Failure & Degradation).

---

### Suite Compatibility (`NFR-COMPAT-*`)

#### NFR-COMPAT-01 — Filigree schema pin test

CI runs a schema-compatibility test against a tagged Filigree release's `GET /api/files/_schema` output, pinning `valid_severities`, `valid_finding_statuses`, `valid_association_types`, and the sort-field lists. Mismatch fails CI.

**Rationale**: Filigree's CHANGELOG flags breaking API changes; a pinned schema-compat test gives code-level protection against drift.
**Verification**: CI job runs; modifying the fixture schema fails the build.
**See**: System Design §9 (Filigree — Schema contract).

#### NFR-COMPAT-02 — Wardline REGISTRY pin test

Clarion's plugin verifies `wardline.core.registry.REGISTRY_VERSION` against a pinned version at startup. Additive-newer passes with warning; major-bump or older falls back to mirror mode with a finding.

**Rationale**: Wardline's REGISTRY is the shared decorator vocabulary; skew produces incorrect Wardline-derived guidance and detection gaps. Version-pinning with graceful degradation matches the prod reality that install versions don't always match.
**Verification**: Unit test with pinned version succeeds; test with mismatched version produces expected finding.
**See**: System Design §2 (Core / Plugin Architecture, Direct REGISTRY import), §11 (Suite Bootstrap, Prerequisites named here).

#### NFR-COMPAT-03 — Anthropic SDK version pin

The Anthropic model-tier mapping (`haiku / sonnet / opus` → concrete model IDs) is pinned against a specific Anthropic SDK version in the build. CI verifies the mapping still resolves at build time.

**Rationale**: Model ID drift is a silent footgun — a `claude-sonnet-4-6` → `claude-sonnet-4-7` rename quietly breaks every LLM call. Pinning and CI-verifying prevents the ship-with-broken-config scenario.
**Verification**: Build succeeds with the pinned SDK; `tier_mapping.*` IDs resolve to valid Anthropic endpoints.
**See**: System Design §5 (Policy Engine, LLM provider abstraction).

---

## Constraints (`CON-*`)

External limits that shape what Clarion v0.1 can do.

### CON-LOOM-01 — Loom federation axiom (solo + pairwise + enrich-only)

Clarion v0.1 must satisfy the Loom federation axiom: useful standalone, composable pairwise with each sibling product, and enrich-only with respect to sibling data. Sibling absence must never change the *meaning* of Clarion's own data; reduced capability is acceptable, altered semantics is not.

**Rationale**: Founding doctrine of the Loom suite ([../../suite/loom.md](../../suite/loom.md) §5). Violating it collapses federation into monolith.
**Verification**: Clarion operates meaningfully with `--no-filigree` and `--no-wardline` (reduced capability, coherent semantics); briefings and catalog structure are unchanged by sibling presence.
**See**: [../../suite/loom.md](../../suite/loom.md), System Design §1 (Context & Boundaries), §11 (Suite Bootstrap).

### CON-FILIGREE-01 — Use Filigree's native scan-results intake (not SARIF)

Clarion emits findings to Filigree via `POST /api/v1/scan-results` using Filigree's flat JSON schema. Extension fields nest under `metadata` (not `properties`). Line fields are `line_start` + `line_end` (not a single `line`). Severity uses Filigree's lowercase enum (`{critical, high, medium, low, info}`).

**Rationale**: Filigree's scan-results endpoint is the production path; deviating requires Filigree work that is out of v0.1 scope. The `metadata` nesting and severity enum are existing Filigree semantics verified by recon.
**See**: System Design §9 (Integrations, Filigree — Wire format).

### CON-FILIGREE-02 — File-registry displacement is deferred to v0.2

Clarion v0.1 does **not** displace Filigree's file registry. The "Clarion owns the file registry" story — Filigree's `registry_backend` flag + pluggable `RegistryProtocol` and Clarion serving as Filigree's registry backend — is deferred to v0.2 per the [Sprint 2 scope amendment §4](../../implementation/sprint-2/scope-amendment-2026-05.md) (WP9-B + WP10). Filigree did ship the `registry_backend` flag during Sprint 2 (ADR-014); Clarion's read-side implementation of `RegistryProtocol` and the associated suite-compat reporting are the v0.2 work.

In v0.1, Clarion operates in shadow-registry mode by default: Clarion owns its entity catalog; Filigree, when present, auto-creates `file_records` under its native rules. The two stores reference the same paths but neither delegates id allocation to the other. This is acceptable because Clarion v0.1's MVP MCP surface (the seven tools — `entity_at`, `find_entity`, `callers_of`, `execution_paths_from`, `summary`, `issues_for`, `neighborhood`) queries Clarion's own SQLite store directly and does not require registry coupling.

**Rationale**: Filigree's four NOT-NULL `file_records(id)` foreign keys + three auto-create paths make registry displacement a schema-surgery integration, not a feature flag — out of v0.1 scope per the scope amendment. Shadow mode preserves v0.1 shipability; full integration lands with the WP9-B/WP10 cluster in v0.2.
**See**: System Design §11 (Suite Bootstrap), [ADR-014 — Filigree `registry_backend` flag and pluggable `RegistryProtocol`](../adr/ADR-014-filigree-registry-backend.md), [Sprint 2 scope amendment §4](../../implementation/sprint-2/scope-amendment-2026-05.md).

### CON-WARDLINE-01 — Wardline owns its REGISTRY

Clarion consumes Wardline's `wardline.core.registry.REGISTRY` and `REGISTRY_VERSION` via direct Python import in v0.1. Wardline's decorator vocabulary is authoritative; Clarion does not redefine it, override it, or ship a parallel vocabulary.

**Rationale**: Principle 5 (observe vs. enforce); Wardline is authoritative for trust-topology vocabulary. Respecting this keeps the suite coherent.
**See**: System Design §2 (Core / Plugin Architecture, Direct REGISTRY import), §11 (Suite Bootstrap, Prerequisites named here).

### CON-ANTHROPIC-01 — Anthropic-only LLM provider in v0.1

Clarion v0.1's LLM provider is Anthropic only. The `LlmProvider` trait exists for testability (`RecordingProvider`) and future extensibility, but the plugin-level prompt protocol assumes Anthropic prompt-caching semantics (four `cache_control` breakpoints at specific segment boundaries). Adding a provider without that caching structure sacrifices cost performance.

**Rationale**: Anthropic's prompt caching is the mechanism that makes elspeth-scale cost tractable; alternative providers either lose caching advantage (pay more) or require prompt-protocol refactoring (v0.3+).
**See**: System Design §5 (Policy Engine, LLM provider abstraction — Honest framing).

### CON-LOCAL-01 — Local-first operational posture

Clarion v0.1 runs entirely on the operator's machine. No mandatory cloud service, no telemetry, no hosted component. The only required network egress is the LLM provider API during summarisation.

**Rationale**: Enterprise-at-lack-of-scale commitment. A hosted component re-introduces the platform-team burden Clarion is designed to avoid.
**See**: System Design §1 (Context & Boundaries).

### CON-RUST-01 — Core implemented in Rust

Clarion's core is implemented in Rust. This is a directive (ADR-001) and not subject to alternatives analysis.

**Rationale**: Primary author's directive. Consequences (single-binary ship, mature ecosystem, plugin interop via subprocess, higher recruiting bar) are accepted.
**See**: System Design §12 (Architecture Decisions — ADR-001), [../adr/ADR-001-rust-for-core.md](../adr/ADR-001-rust-for-core.md).

### CON-SQLITE-01 — SQLite is the v0.1 store

Clarion v0.1 uses SQLite as its persistence layer. Kuzu, DuckDB, and custom graph stores are considered and rejected for v0.1 (detailed-design §4); repository layer is kept thin enough to swap post-v0.3 if profiling demands.

**Rationale**: SQLite is single-file, mature, debuggable with standard tooling, and handles JSON1 + FTS5 without giving up query ergonomics. Matches the "enterprise at lack of scale" commitment.
**See**: System Design §4 (Storage — Technology).

---

## Non-Goals (`NG-*`)

These are Clarion v0.1's authoritative non-goals — what the product explicitly does NOT do. Items originating as deferrals from the pre-restructure design have been normalised into this list.

### NG-01 — Not a linter or pattern-rule scanner

Clarion does not implement pattern-rule scanning of the kind Wardline's enforcer owns. Structural findings (`CLA-FACT-*`) emit observations; they do not enforce compliance.

**Why**: Principle 5 (observe vs enforce). Wardline's territory.

### NG-02 — Not a workflow / issue tracker

Clarion does not own issues, triage state, or workflow transitions. Filigree is authoritative.

**Why**: Loom federation — Filigree's territory.

### NG-03 — Not an IDE or editor

Clarion is invoked by existing editors (via MCP clients like Claude Code). It does not ship its own editor surface.

**Why**: Out of scope; users already have editors.

### NG-04 — Not a code-search tool

Clarion ships `search_*` MCP tools (`search_structural`, `search_semantic`) scoped to consult navigation. These are not a grep-replacement — external tools (ripgrep, GitHub code search, IDE search) remain the right answer for text-hunting.

**Why**: Search serves consult-mode navigation, not general text search.

### NG-05 — Not a dataflow / taint analyser

Clarion does not implement dataflow or taint analysis. Wardline's concern; Clarion surfaces Wardline's findings via MCP.

**Why**: Principle 5 + Wardline's territory.

### NG-06 — Not a hosted / cloud service

Clarion does not offer a hosted deployment, a cloud-managed service, or multi-tenant SaaS. Local-first, single-binary only.

**Why**: `CON-LOCAL-01`; enterprise-at-lack-of-scale commitment.

### NG-07 — Not a change executor (Shuttle's territory)

Clarion does not execute code changes, propose edits, apply patches, or run tests as part of an edit workflow. Transactional scoped change execution is Shuttle's territory (see [../../suite/loom.md](../../suite/loom.md)), not Clarion's.

**Why**: Loom federation — Shuttle, when built, owns this domain. Clarion observes; Shuttle executes.

### NG-08 — Not a code transformer / refactoring suggester

Clarion does not rewrite source, propose refactorings, or emit code patches. "Clarion observes, it doesn't rewrite."

**Why**: Out of roadmap (never, not just deferred).

### NG-09 — Not a CI-time enforcer

Clarion emits findings to Filigree and structural signals to operators; it does not block CI pipelines, fail builds, or enforce gates. Enforcement belongs to Wardline (at commit cadence) and Filigree's triage policies.

**Why**: Principle 5 + Loom federation.

### NG-10 — Not a real-time file watcher

Clarion does not watch the filesystem and re-analyse on change. LLM spend model doesn't support it; `clarion analyze` is explicitly batch.

**Why**: Cost posture.

### NG-11 — Not a multi-branch analyser

Clarion analyses one tree at a time. Comparing analyses across branches is an operator-level concern (running `clarion analyze` on each branch and diffing outputs).

**Why**: Out of scope for v0.1; overhead outweighs value.

### NG-12 — Deferred: incremental / diff-driven analysis

Incremental re-analysis (git-diff driven; per-file cache invalidation; partial re-clustering; entity-level staleness tracking) is deferred to v0.2. V0.1 always runs the full pipeline (resumable, but not incremental).

**Why**: Incremental correctness is subtle; v0.1 prefers full runs with aggressive caching.

### NG-13 — Deferred: static wiki UI

Semi-dynamic wiki (HTML served by `clarion serve`, live guidance editing, live finding lists, live filigree cross-links, consult entry points) is deferred to v0.2. V0.1 ships catalog artefacts (JSON + markdown) only.

**Why**: Out of v0.1 scope; catalog artefacts cover the "I want to read the output" case.

### NG-14 — Deferred: `EntityAlias` rename tracking

Symbol-rename-without-file-move detaches cross-tool references in v0.1. Explicit `EntityAlias` table (with rename detection heuristics) is deferred to v0.2. V0.1 ships `clarion analyze --repair-aliases <old> <new>` as a manual workaround.

**Why**: Rename detection is a proper research problem (AST similarity + git rename + name similarity); v0.1 accepts the limitation and names it in release notes.

### NG-15 — Deferred: second language plugin

Clarion v0.1 ships only a Python plugin. A second language plugin (Java, Rust, TypeScript) is v0.2+.

**Why**: Proving the plugin protocol with one language before extending is prudent; elspeth is Python.

### NG-16 — Deferred: plugin hash-pinning

Malicious-plugin threat (§10) is acknowledged; plugin binary hash-pinning in `plugins.toml` is v0.2. V0.1 operators install only from trusted sources.

**Why**: Security scope limited; trusted-source-only posture documented.

### NG-17 — Deferred: triage-feedback loop from Filigree

Filigree's per-rule suppression rates and time-to-close metrics flowing back as Clarion rule-priority modifiers (e.g., rule suppressed >N% of time → `CLA-INFRA-RULE-LOW-VALUE`) is deferred to v0.2. V0.1 reads triage state one-way (for briefing enrichment, `REQ-BRIEFING-05`) but doesn't adjust emission policy.

**Why**: Triage-feedback requires stable triage data over months; v0.1 establishes the primary emission path first.

### NG-18 — Deferred: Wardline-delta, quality, security cross-cutting analyses

Phase-7 cross-cutting LLM analyses (Wardline-delta surface, quality analysis, security analysis) are v0.2. V0.1's Phase 7 is Wardline state ingest + structural findings only.

**Why**: Cross-cutting analyses depend on stable primary outputs; v0.1 establishes those.

### NG-19 — Deferred: BAR (built, assembled, runtime) subsystem awareness

Build / assembly / runtime subsystem classification is v0.2. V0.1 clustering is purely structural (imports + calls).

**Why**: Non-trivial; out of v0.1 scope.

### NG-20 — Deferred: Wardline HTTP state-pull from Clarion

Wardline consuming Clarion's HTTP read API via its own HTTP client + `ProjectIndex` abstraction is v0.2+. V0.1: Wardline keeps re-scanning; Clarion serves the API for future consumption.

**Why**: Wardline-side refactor is significant; not a v0.1 timeline.

### NG-21 — Deferred: server-side per-entity dedup in Filigree

Clarion's `mark_unseen=true` workaround (`REQ-FINDING-06`) is the v0.1 dedup policy. Server-side per-entity dedup in Filigree's scan-results intake is a v0.2+ Filigree feature.

**Why**: Filigree work; not a Clarion v0.1 deliverable.

### NG-22 — Deferred: Clarion-native SARIF export

Clarion emits findings to Filigree (native scan-results), not to SARIF. External SARIF export for GitHub code-scanning / CI reporters is v0.2; v0.1 relies on Wardline's existing SARIF output for that surface.

**Why**: SARIF generation is non-trivial; v0.1 focuses on Filigree intake.

### NG-23 — Deferred: coverage.xml ingestion

pytest-coverage → per-entity coverage findings is v0.2. V0.1 does not ingest coverage data.

**Why**: Out of v0.1 scope.

### NG-24 — Deferred: advanced git analysis

Churn is captured at file level (`REQ-CATALOG-04`). Co-change graphs, authorship clustering, and deep git-log analysis beyond per-file properties are v0.2+.

**Why**: Out of v0.1 scope.

### NG-25 — Deferred: Wardline annotation descriptor consumption

Wardline shipping a YAML/JSON descriptor of its REGISTRY (instead of requiring direct Python import) is v0.2. V0.1's Python-only plugin can import directly; non-Python plugins in v0.2+ will need the descriptor.

**Why**: Python-direct import works for v0.1's single plugin; descriptor is a cross-language requirement for later.

---

**End of Clarion v1.0 requirements specification.**
